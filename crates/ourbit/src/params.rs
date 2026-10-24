use crate::error;
use serde_json::{Map, Value};
use tradeslope_sdk::BackendResult;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarketKind {
    Spot,
    Futures,
}

impl MarketKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Futures => "futures",
        }
    }
}

pub(crate) struct Params(Map<String, Value>);

impl Params {
    pub fn new(value: Value) -> BackendResult<Self> {
        match value {
            Value::Object(map) if map.len() <= 32 => Ok(Self(map)),
            _ => Err(error(
                "invalid_params",
                "params must be an object with at most 32 fields",
            )),
        }
    }

    pub fn string(&mut self, name: &str) -> BackendResult<Option<String>> {
        match self.0.remove(name) {
            None => Ok(None),
            Some(Value::String(s))
                if !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_graphic()) =>
            {
                Ok(Some(s))
            }
            _ => Err(error(
                "invalid_params",
                &format!("{name} must be a nonempty ASCII string (max 128)"),
            )),
        }
    }

    pub fn required_string(&mut self, name: &str) -> BackendResult<String> {
        self.string(name)?
            .ok_or_else(|| error("invalid_params", &format!("{name} is required")))
    }

    pub fn integer(&mut self, name: &str, min: i64, max: i64) -> BackendResult<Option<i64>> {
        match self.0.remove(name) {
            None => Ok(None),
            Some(v) => v
                .as_i64()
                .filter(|n| (min..=max).contains(n))
                .map(Some)
                .ok_or_else(|| {
                    error(
                        "invalid_params",
                        &format!("{name} must be an integer in {min}..={max}"),
                    )
                }),
        }
    }

    pub fn required_integer(&mut self, name: &str, min: i64, max: i64) -> BackendResult<i64> {
        self.integer(name, min, max)?
            .ok_or_else(|| error("invalid_params", &format!("{name} is required")))
    }

    pub fn limit(&mut self, max: i64) -> BackendResult<usize> {
        // All operations share the SDK bound; lower exchange bounds clamp, including defaults.
        Ok(self.integer("limit", 1, 1000)?.unwrap_or(200).min(max) as usize)
    }

    pub fn decimal(&mut self, name: &str, required: bool) -> BackendResult<Option<String>> {
        let value = match self.0.remove(name) {
            None if !required => return Ok(None),
            Some(Value::String(s)) => s,
            Some(Value::Number(n)) => n.to_string(),
            _ => {
                return Err(error(
                    "invalid_params",
                    &format!("{name} must be a positive decimal"),
                ));
            }
        };
        // Preserve input decimal precision. No exponent, rounding, sign, whitespace or NaN.
        let pieces: Vec<_> = value.split('.').collect();
        let valid = value.len() <= 64
            && pieces.len() <= 2
            && pieces
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            && value.parse::<f64>().is_ok_and(|n| n.is_finite() && n > 0.0);
        if !valid {
            return Err(error(
                "invalid_params",
                &format!("{name} must be a finite positive plain decimal"),
            ));
        }
        Ok(Some(value))
    }

    pub fn symbol(&mut self, market: MarketKind, required: bool) -> BackendResult<Option<String>> {
        let Some(raw) = self.string("symbol")? else {
            return if required {
                Err(error("invalid_params", "symbol is required"))
            } else {
                Ok(None)
            };
        };
        let s = raw.to_ascii_uppercase();
        let parts: Vec<_> = s.split('_').collect();
        if parts.len() > 2
            || parts
                .iter()
                .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_alphanumeric()))
            || s.len() > 48
            || s.len() < 3
        {
            return Err(error(
                "invalid_params",
                "symbol must be a compact or BASE_QUOTE alphanumeric pair",
            ));
        }
        let normalized = if market == MarketKind::Spot {
            parts.concat()
        } else if parts.len() == 2 {
            s
        } else {
            // Explicit well-known settlement suffixes only, never guess a base/quote midpoint.
            let quote = ["USDT", "USDC", "USD", "BTC", "ETH"]
                .into_iter()
                .find(|q| s.ends_with(q) && s.len() > q.len())
                .ok_or_else(|| {
                    error(
                        "invalid_params",
                        "Use BASE_QUOTE for an ambiguous futures symbol",
                    )
                })?;
            format!("{}_{}", &s[..s.len() - quote.len()], quote)
        };
        Ok(Some(normalized))
    }

    pub fn times(&mut self) -> BackendResult<(Option<i64>, Option<i64>)> {
        // Public SDK input uses seconds throughout; conversion to ms is explicit at API boundaries.
        let start = self.integer("start", 0, 253402300799)?;
        let end = self.integer("end", 0, 253402300799)?;
        if let (Some(start), Some(end)) = (start, end)
            && start > end
        {
            return Err(error("invalid_params", "start must not exceed end"));
        }
        Ok((start, end))
    }

    pub fn finish(self) -> BackendResult<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(error("invalid_params", "Unknown or inapplicable parameter"))
        }
    }
}
