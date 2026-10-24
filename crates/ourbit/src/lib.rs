//! Ourbit REST backend. See the crate README for operation schemas and restrictions.
mod params;
mod private;
mod public;
mod runtime;
mod schema;
mod transport;

use params::{MarketKind, Params};
use serde_json::Value;
use std::{env, fmt};
use tradeslope_sdk::{Access, Backend, BackendError, BackendManifest, BackendResult, Operation};
use transport::Transport;

pub const SPOT_ENDPOINT: &str = "https://api.ourbit.com";
pub const FUTURES_ENDPOINT: &str = "https://futures.ourbit.com";

/// Never serialized or included in diagnostics. Supply secrets in memory or via environment.
#[derive(Clone)]
pub struct Credentials {
    pub(crate) key: String,
    pub(crate) secret: String,
}

impl Credentials {
    pub fn new(key: impl Into<String>, secret: impl Into<String>) -> BackendResult<Self> {
        let value = Self {
            key: key.into(),
            secret: secret.into(),
        };
        if value.key.is_empty()
            || value.secret.is_empty()
            || value.key.len() > 512
            || value.secret.len() > 512
            || !value.key.bytes().all(|b| b.is_ascii_graphic())
            || !value.secret.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(error(
                "configuration",
                "Credentials must be nonempty printable ASCII",
            ));
        }
        Ok(value)
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials([REDACTED])")
    }
}

/// A synchronous, thread-safe backend; call off the UI/event-loop thread.
pub struct Ourbit {
    http: Transport,
    spot_credentials: Option<Credentials>,
    futures_credentials: Option<Credentials>,
    worker: runtime::Worker,
}

impl fmt::Debug for Ourbit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ourbit { configuration: [REDACTED] }")
    }
}

impl Ourbit {
    /// In-memory credentials with fixed official endpoints; no disk or environment writes.
    pub fn with_credentials(api_key: String, api_secret: String) -> BackendResult<Self> {
        Self::with_endpoints(
            SPOT_ENDPOINT,
            FUTURES_ENDPOINT,
            Some(Credentials::new(api_key, api_secret)?),
            None,
        )
    }

    /// Fixed production HTTPS endpoints; endpoint environment variables are deliberately ignored.
    /// Futures credentials fall back to OURBIT_API_KEY/OURBIT_API_SECRET.
    pub fn from_env() -> BackendResult<Self> {
        fn pair(key: &str, secret: &str) -> BackendResult<Option<Credentials>> {
            let read = |name| match env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(env::VarError::NotPresent) => Ok(None),
                Err(_) => Err(error(
                    "configuration",
                    "Credential environment value is not Unicode",
                )),
            };
            match (read(key)?, read(secret)?) {
                (None, None) => Ok(None),
                (Some(k), Some(s)) => Credentials::new(k, s).map(Some),
                _ => Err(error(
                    "configuration",
                    "Both API key and secret must be supplied",
                )),
            }
        }
        Self::with_endpoints(
            SPOT_ENDPOINT,
            FUTURES_ENDPOINT,
            pair("OURBIT_API_KEY", "OURBIT_API_SECRET")?,
            pair("OURBIT_FUTURES_API_KEY", "OURBIT_FUTURES_API_SECRET")?,
        )
    }

    /// Explicit endpoint override for fixtures or a trusted gateway.
    /// HTTPS is required except for numeric loopback HTTP fixtures. Never use untrusted gateways:
    /// private calls send credentials to the configured origin. No redirects or URL userinfo.
    pub fn with_endpoints(
        spot: &str,
        futures: &str,
        spot_credentials: Option<Credentials>,
        futures_credentials: Option<Credentials>,
    ) -> BackendResult<Self> {
        Ok(Self {
            http: Transport::new(spot, futures)?,
            spot_credentials,
            futures_credentials,
            worker: runtime::Worker::default(),
        })
    }

    fn credentials(&self, market: MarketKind) -> BackendResult<Credentials> {
        if let Some(selected) = self.worker.active_account()? {
            let profile = selected
                .ok_or_else(|| error("credentials_required", "No runtime account is selected"))?;
            return runtime::resolve_credentials(&profile, market, |name| env::var(name).ok());
        }
        (match market {
            MarketKind::Spot => self.spot_credentials.as_ref(),
            MarketKind::Futures => self
                .futures_credentials
                .as_ref()
                .or(self.spot_credentials.as_ref()),
        })
        .cloned()
        .ok_or_else(|| {
            error(
                "credentials_required",
                "This operation requires API credentials",
            )
        })
    }
}

const PUBLIC: &[&str] = &[
    "markets",
    "ticker",
    "candles",
    "order_book",
    "trades",
    "health",
    "funding",
];
const PRIVATE: &[&str] = &[
    "balances",
    "positions",
    "open_orders",
    "order_history",
    "user_trades",
];
const TRADE: &[&str] = &["place_order", "cancel_order", "cancel_all"];

impl Backend for Ourbit {
    fn sources(&self) -> Vec<tradeslope_sdk::DataSource> {
        schema::sources()
    }

    fn settings(&self) -> tradeslope_sdk::runtime::ExtensionSettings {
        runtime::settings()
    }

    fn validate_settings(&self, settings: &Value) -> BackendResult<()> {
        runtime::validate_settings(settings)
    }

    fn start(&self, services: tradeslope_sdk::runtime::BackendServices) -> BackendResult<()> {
        self.worker.start(self.http.clone(), services)
    }

    fn stop(&self) -> BackendResult<()> {
        self.worker.stop()
    }

    fn manifest(&self) -> BackendManifest {
        let operations = PUBLIC
            .iter()
            .map(|id| (*id, Access::Public))
            .chain(PRIVATE.iter().map(|id| (*id, Access::PrivateRead)))
            .chain(TRADE.iter().map(|id| (*id, Access::Trade)))
            .map(|(id, access)| Operation {
                id: id.into(),
                title: id.replace('_', " "),
                kind: if TRADE.contains(&id) {
                    tradeslope_sdk::OperationKind::Action
                } else {
                    tradeslope_sdk::OperationKind::Query
                },
                parameters: schema::parameters(id),
                output: schema::output(id),
                access,
            })
            .collect();
        BackendManifest {
            id: "ourbit".into(),
            title: "Ourbit".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            operations,
        }
    }

    fn execute(&self, operation: &str, params: Value) -> BackendResult<Value> {
        if !PUBLIC.contains(&operation)
            && !PRIVATE.contains(&operation)
            && !TRADE.contains(&operation)
        {
            return Err(error("unsupported_operation", "Unknown Ourbit operation"));
        }
        let mut params = Params::new(params)?;
        let market = match params.required_string("market")?.as_str() {
            "spot" => MarketKind::Spot,
            "futures" => MarketKind::Futures,
            _ => return Err(error("invalid_params", "market must be spot or futures")),
        };
        if PUBLIC.contains(&operation) {
            self.public(operation, market, params)
        } else {
            self.private(operation, market, params)
        }
    }
}

pub(crate) fn error(code: &str, message: &str) -> BackendError {
    BackendError::new(code, message)
}

#[cfg(test)]
mod tests;
