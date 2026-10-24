use crate::{Credentials, error, params::MarketKind};
use hmac::{Hmac, Mac};
use reqwest::{Method, Url, blocking::Client, header::HeaderValue};
use serde_json::Value;
use sha2::Sha256;
use std::{
    io::Read,
    net::IpAddr,
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tradeslope_sdk::BackendResult;

pub(crate) const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(crate) struct Transport {
    client: Client,
    spot: Url,
    futures: Url,
}

// Shared across every instance, both markets, and public/private traffic in this process.
struct Gate {
    next: Instant,
    blocked: Option<Instant>,
    suspended: bool,
}
static GATE: OnceLock<Mutex<Gate>> = OnceLock::new();

fn gate() -> &'static Mutex<Gate> {
    GATE.get_or_init(|| {
        Mutex::new(Gate {
            next: Instant::now(),
            blocked: None,
            suspended: false,
        })
    })
}

fn throttle(spacing: Duration) -> BackendResult<()> {
    let mut state = gate()
        .lock()
        .map_err(|_| error("internal", "Rate limiter unavailable"))?;
    if state.suspended {
        return Err(error(
            "rate_limited",
            "Provider requested an unbounded cooldown; restart the backend later",
        ));
    }
    let next = state.blocked.map_or(state.next, |b| b.max(state.next));
    let wait = next.saturating_duration_since(Instant::now());
    if wait > MAX_WAIT {
        return Err(error(
            "rate_limited",
            "Provider cooldown active; try again later",
        ));
    }
    // Hold lock while sleeping to avoid a burst of concurrent requests at the same instant.
    thread::sleep(wait);
    state.next = Instant::now() + spacing;
    Ok(())
}

// Health checks share the foreground gate but never wait behind it.
fn health_slot() -> BackendResult<()> {
    let mut state = gate()
        .try_lock()
        .map_err(|_| error("rate_limited", "Health check deferred by rate limiter"))?;
    let now = Instant::now();
    if state.suspended || state.blocked.map_or(state.next, |b| b.max(state.next)) > now {
        return Err(error(
            "rate_limited",
            "Health check deferred by rate limiter",
        ));
    }
    state.next = now + Duration::from_millis(210);
    Ok(())
}

fn cooldown(value: Option<&str>) -> Duration {
    let wait = value
        .and_then(|s| {
            s.parse::<u64>().ok().map(Duration::from_secs).or_else(|| {
                httpdate::parse_http_date(s)
                    .ok()
                    .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
            })
        })
        .unwrap_or(Duration::from_secs(1));
    if let Ok(mut state) = gate().lock() {
        if let Some(until) = Instant::now().checked_add(wait) {
            state.blocked = Some(state.blocked.map_or(until, |previous| previous.max(until)));
        } else {
            state.suspended = true;
        }
    }
    wait
}

fn endpoint(value: &str) -> BackendResult<Url> {
    let url = Url::parse(value).map_err(|_| error("configuration", "Invalid endpoint URL"))?;
    let local = url
        .host_str()
        .and_then(|s| s.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    if !(url.scheme() == "https" || url.scheme() == "http" && local)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(error(
            "configuration",
            "Endpoint must be an HTTPS origin (or numeric loopback HTTP fixture)",
        ));
    }
    Ok(url)
}

pub(crate) fn encode(fields: &[(String, String)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().map(|(k, v)| (k, v)))
        .finish()
}

pub(crate) fn sign(secret: &str, text: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(text.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn now_millis() -> BackendResult<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .ok_or_else(|| error("clock", "System clock is invalid"))
}

fn secret_header(value: &str) -> BackendResult<HeaderValue> {
    let mut header = HeaderValue::from_str(value)
        .map_err(|_| error("configuration", "Credential is not a valid HTTP header"))?;
    header.set_sensitive(true);
    Ok(header)
}

impl Transport {
    pub fn new(spot: &str, futures: &str) -> BackendResult<Self> {
        Ok(Self {
            client: Client::builder()
                .use_rustls_tls()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                // Do not send credentials through ambient system/environment proxies.
                .no_proxy()
                .user_agent("tradeslope-ourbit/0.1")
                .build()
                .map_err(|_| error("configuration", "Unable to initialize TLS HTTP client"))?,
            spot: endpoint(spot)?,
            futures: endpoint(futures)?,
        })
    }

    pub fn request(
        &self,
        market: MarketKind,
        method: Method,
        path: &str,
        fields: Vec<(String, String)>,
        body: Option<Value>,
        credentials: Option<&Credentials>,
    ) -> BackendResult<Value> {
        self.request_inner(market, method, path, fields, body, credentials, false)
    }

    /// Public-only, one attempt with a one-second request/body deadline.
    pub fn health(&self) -> BackendResult<()> {
        let value = self.request_inner(
            MarketKind::Spot,
            Method::GET,
            "/api/v3/ping",
            Vec::new(),
            None,
            None,
            true,
        )?;
        if !value.is_object() {
            return Err(error("invalid_response", "Invalid ping response"));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn request_inner(
        &self,
        market: MarketKind,
        method: Method,
        path: &str,
        fields: Vec<(String, String)>,
        body: Option<Value>,
        credentials: Option<&Credentials>,
        health: bool,
    ) -> BackendResult<Value> {
        let read = method == Method::GET;
        for attempt in 0..if health { 1 } else { 2 } {
            // Account information is explicitly limited to 2/s in the spot docs.
            let spacing = if path == "/api/v3/account" { 510 } else { 210 };
            if health {
                health_slot()?;
            } else {
                throttle(Duration::from_millis(spacing))?;
            }
            let mut url = match market {
                MarketKind::Spot => self.spot.clone(),
                MarketKind::Futures => self.futures.clone(),
            };
            url.set_path(path);
            let mut fields = fields.clone();
            if market == MarketKind::Futures {
                fields.sort_by(|a, b| a.0.cmp(&b.0));
            }
            let mut query = encode(&fields).replace('+', "%20");
            let body = body.as_ref().map(Value::to_string);
            let timestamp = now_millis()?.to_string();
            let mut builder;
            match (market, credentials) {
                (MarketKind::Spot, Some(c)) => {
                    if !query.is_empty() {
                        query.push('&');
                    }
                    query.push_str(&format!("recvWindow=5000&timestamp={timestamp}"));
                    let signature = sign(&c.secret, &query);
                    query.push_str("&signature=");
                    query.push_str(&signature);
                    url.set_query(Some(&query));
                    builder = self
                        .client
                        .request(method.clone(), url)
                        .header("X-OURBIT-APIKEY", secret_header(&c.key)?);
                }
                (MarketKind::Futures, Some(c)) => {
                    if !query.is_empty() {
                        url.set_query(Some(&query));
                    }
                    let payload = body.as_deref().unwrap_or(&query);
                    let signature = sign(&c.secret, &format!("{}{timestamp}{payload}", c.key));
                    builder = self
                        .client
                        .request(method.clone(), url)
                        .header("ApiKey", secret_header(&c.key)?)
                        .header("Request-Time", timestamp)
                        .header("Signature", secret_header(&signature)?)
                        .header("Recv-Window", "5");
                }
                (_, None) => {
                    if !query.is_empty() {
                        url.set_query(Some(&query));
                    }
                    builder = self.client.request(method.clone(), url);
                }
            }
            if let Some(body) = body {
                builder = builder
                    .header("Content-Type", "application/json")
                    .body(body);
            } else if credentials.is_some() {
                builder = builder.header("Content-Type", "application/json");
            }
            if health {
                builder = builder.timeout(Duration::from_secs(1));
            }
            let response = match builder.send() {
                Ok(response) => response,
                Err(_) if !health && read && attempt == 0 => {
                    thread::sleep(Duration::from_millis(250));
                    continue;
                }
                Err(_) => {
                    return Err(error(
                        "transport",
                        if read {
                            "HTTP request failed"
                        } else {
                            "HTTP request failed; mutation outcome may be unknown; reconcile before resubmitting"
                        },
                    ));
                }
            };
            let status = response.status();
            if status.as_u16() == 429 || status.as_u16() == 418 {
                let delay = cooldown(
                    response
                        .headers()
                        .get("Retry-After")
                        .and_then(|h| h.to_str().ok()),
                );
                if !health && read && attempt == 0 && status.as_u16() == 429 && delay <= MAX_WAIT {
                    continue;
                }
                return Err(error(
                    "rate_limited",
                    "Provider rate limit; mutation outcome may be unknown",
                ));
            }
            if !health && status.is_server_error() && read && attempt == 0 {
                thread::sleep(Duration::from_millis(250));
                continue;
            }
            if !status.is_success() {
                return Err(error(
                    "http_error",
                    if read {
                        "Provider returned a non-success HTTP status"
                    } else {
                        "Provider returned a non-success status; reconcile mutation outcome before resubmitting"
                    },
                ));
            }
            if response
                .content_length()
                .is_some_and(|size| size > MAX_BYTES as u64)
            {
                return Err(error(
                    "response_too_large",
                    "Provider response exceeds 8 MiB",
                ));
            }
            let mut bytes = Vec::new();
            response
                .take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| {
                    error(
                        "transport",
                        "Unable to read response; reconcile any mutation outcome",
                    )
                })?;
            if bytes.len() > MAX_BYTES {
                return Err(error(
                    "response_too_large",
                    "Provider response exceeds 8 MiB",
                ));
            }
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                error(
                    "invalid_response",
                    "Provider returned invalid JSON; reconcile any mutation outcome",
                )
            })?;
            if !value.is_object() && !value.is_array() {
                return Err(error(
                    "invalid_response",
                    "Provider returned a non-container response; reconcile any mutation outcome",
                ));
            }
            check_envelope(&value)?;
            if market == MarketKind::Futures {
                data(&value, market)?;
            }
            return Ok(value);
        }
        unreachable!()
    }
}

pub(crate) fn check_envelope(value: &Value) -> BackendResult<()> {
    let bad_code = value.get("code").is_some_and(|code| {
        !matches!(
            code.as_i64().or_else(|| code.as_str()?.parse().ok()),
            Some(0 | 200)
        )
    });
    if value.get("success") == Some(&Value::Bool(false)) || bad_code {
        // Never surface upstream messages, codes, bodies or URLs: they may echo secrets.
        return Err(error("provider_error", "Ourbit rejected the request"));
    }
    Ok(())
}

pub(crate) fn data(value: &Value, market: MarketKind) -> BackendResult<&Value> {
    if market == MarketKind::Futures {
        if value.get("success") != Some(&Value::Bool(true)) {
            return Err(error(
                "invalid_response",
                "Missing futures success envelope",
            ));
        }
        value
            .get("data")
            .ok_or_else(|| error("invalid_response", "Missing provider data"))
    } else {
        Ok(value)
    }
}
