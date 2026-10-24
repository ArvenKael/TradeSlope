use crate::{
    Ourbit, error,
    params::{MarketKind, Params},
    transport::data,
};
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tradeslope_sdk::{BackendResult, Candle, Market, Ticker};

pub(crate) fn number(v: &Value) -> BackendResult<f64> {
    let n = v
        .as_f64()
        .or_else(|| v.as_str()?.parse().ok())
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or_else(|| {
            error(
                "invalid_response",
                "Missing, negative or non-finite provider number",
            )
        })?;
    Ok(n)
}

fn integer(v: &Value) -> BackendResult<i64> {
    v.as_i64()
        .or_else(|| v.as_str()?.parse().ok())
        .filter(|n| *n >= 0)
        .ok_or_else(|| error("invalid_response", "Missing or invalid provider timestamp"))
}

fn text(v: &Value) -> BackendResult<&str> {
    v.as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error("invalid_response", "Missing provider string"))
}

fn array(v: &Value) -> BackendResult<&Vec<Value>> {
    v.as_array()
        .ok_or_else(|| error("invalid_response", "Missing provider array"))
}

fn optional_number(v: &Value) -> BackendResult<Option<f64>> {
    if v.is_null() {
        Ok(None)
    } else {
        number(v).map(Some)
    }
}

fn serialize(value: impl serde::Serialize) -> BackendResult<Value> {
    serde_json::to_value(value)
        .map_err(|_| error("invalid_response", "Unable to encode normalized response"))
}

fn interval(input: &str, market: MarketKind) -> BackendResult<&'static str> {
    let (spot, futures) = match input {
        "1m" => ("1m", "Min1"),
        "5m" => ("5m", "Min5"),
        "15m" => ("15m", "Min15"),
        "30m" => ("30m", "Min30"),
        "1h" => ("60m", "Min60"),
        "4h" => ("4h", "Hour4"),
        "1d" => ("1d", "Day1"),
        "1w" if market == MarketKind::Futures => ("", "Week1"),
        "1w" => {
            return Err(error(
                "unsupported_capability",
                "Ourbit spot REST does not support weekly candles",
            ));
        }
        "1M" => ("1M", "Month1"),
        _ => return Err(error("invalid_params", "Unsupported candle interval")),
    };
    Ok(if market == MarketKind::Spot {
        spot
    } else {
        futures
    })
}

// Spot ignores endTime unless startTime is also present. Anchor the window to
// candle opens, not the local clock, so a partial final candle counts as one.
fn spot_history_start(end: i64, interval: &str, limit: usize) -> i64 {
    if interval == "1M" {
        fn year_days(year: i64) -> i64 {
            fn leaps_before(year: i64) -> i64 {
                let y = year - 1;
                y / 4 - y / 100 + y / 400
            }
            365 * (year - 1970) + leaps_before(year) - leaps_before(1970)
        }
        fn month_days(year: i64) -> [i64; 12] {
            let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
            [
                31,
                if leap { 29 } else { 28 },
                31,
                30,
                31,
                30,
                31,
                31,
                30,
                31,
                30,
                31,
            ]
        }
        let day = end / 86400;
        let mut year = 1970 + day / 365;
        while year_days(year) > day {
            year -= 1;
        }
        let months = month_days(year);
        let mut remaining = day - year_days(year);
        let mut month = 0;
        while remaining >= months[month] {
            remaining -= months[month];
            month += 1;
        }
        let index = (year - 1970) * 12 + month as i64;
        let first = (index - (limit as i64 - 1)).max(0);
        let year = 1970 + first / 12;
        let month = first % 12;
        let months = month_days(year);
        return (year_days(year) + months[..month as usize].iter().sum::<i64>()) * 86400;
    }
    let seconds = match interval {
        "1m" => 60,
        "5m" => 300,
        "15m" => 900,
        "30m" => 1800,
        "60m" => 3600,
        "4h" => 14400,
        "1d" => 86400,
        _ => unreachable!("validated spot interval"),
    };
    (end / seconds * seconds - (limit as i64 - 1) * seconds).max(0)
}

impl Ourbit {
    pub(crate) fn public(
        &self,
        op: &str,
        market: MarketKind,
        mut p: Params,
    ) -> BackendResult<Value> {
        if op == "funding" && market == MarketKind::Spot {
            return Err(error(
                "unsupported_capability",
                "Funding is only supported for futures",
            ));
        }
        let symbol = if matches!(op, "markets" | "health") {
            None
        } else {
            p.symbol(market, true)?
        };
        let mut fields = Vec::new();
        let mut limit = 200;
        if let Some(s) = &symbol {
            fields.push(("symbol".into(), s.clone()));
        }
        let path = match (market, op) {
            (MarketKind::Spot, "health") => "/api/v3/ping".to_string(),
            (MarketKind::Futures, "health") => "/api/v1/contract/ping".to_string(),
            (MarketKind::Spot, "markets") => "/api/v3/exchangeInfo".to_string(),
            (MarketKind::Futures, "markets") => "/api/v1/contract/detail".to_string(),
            (MarketKind::Spot, "ticker") => "/api/v3/ticker/24hr".to_string(),
            (MarketKind::Futures, "ticker") => "/api/v1/contract/ticker".to_string(),
            (MarketKind::Futures, "funding") => {
                fields.clear();
                format!(
                    "/api/v1/contract/funding_rate/{}",
                    symbol.as_deref().unwrap()
                )
            }
            (MarketKind::Spot, "order_book" | "trades") => {
                limit = p.limit(1000)?;
                fields.push(("limit".into(), limit.to_string()));
                if op == "order_book" {
                    "/api/v3/depth"
                } else {
                    "/api/v3/trades"
                }
                .into()
            }
            (MarketKind::Futures, "order_book" | "trades") => {
                limit = p.limit(if op == "order_book" { 1000 } else { 100 })?;
                fields.clear();
                fields.push(("limit".into(), limit.to_string()));
                format!(
                    "/api/v1/contract/{}/{}",
                    if op == "order_book" { "depth" } else { "deals" },
                    symbol.as_deref().unwrap()
                )
            }
            (_, "candles") => {
                let candle_interval = interval(&p.required_string("interval")?, market)?;
                limit = p.limit(1000)?;
                let (start, end) = p.times()?;
                fields.push(("interval".into(), candle_interval.into()));
                if market == MarketKind::Spot {
                    fields.push(("limit".into(), limit.to_string()));
                    let start = start
                        .or_else(|| end.map(|end| spot_history_start(end, candle_interval, limit)));
                    if let Some(start) = start {
                        fields.push(("startTime".into(), (start * 1000).to_string()));
                    }
                    if let Some(end) = end {
                        fields.push(("endTime".into(), (end * 1000).to_string()));
                    }
                    "/api/v3/klines".into()
                } else {
                    fields.retain(|(key, _)| key != "symbol");
                    // Futures returns a bounded provider window, not a row-count parameter.
                    // Omit absent times so "latest" follows exchange time, not a skewed local clock.
                    if let Some(start) = start {
                        fields.push(("start".into(), start.to_string()));
                    }
                    if let Some(end) = end {
                        fields.push(("end".into(), end.to_string()));
                    }
                    format!("/api/v1/contract/kline/{}", symbol.as_deref().unwrap())
                }
            }
            _ => {
                return Err(error(
                    "unsupported_operation",
                    "Unsupported public operation",
                ));
            }
        };
        p.finish()?;
        let response = self
            .http
            .request(market, Method::GET, &path, fields, None, None)?;
        let data = data(&response, market)?;
        match op {
            "health" => {
                if market == MarketKind::Spot && !data.is_object() {
                    return Err(error("invalid_response", "Invalid ping response"));
                }
                Ok(json!({"ok":true,"market":market.name()}))
            }
            "markets" => {
                let rows = if market == MarketKind::Spot {
                    array(&data["symbols"])?
                } else {
                    array(data)?
                };
                rows.iter()
                    .map(|row| {
                        serialize(Market {
                            symbol: text(&row["symbol"])?.into(),
                            base: text(
                                &row[if market == MarketKind::Spot {
                                    "baseAsset"
                                } else {
                                    "baseCoin"
                                }],
                            )?
                            .into(),
                            quote: text(
                                &row[if market == MarketKind::Spot {
                                    "quoteAsset"
                                } else {
                                    "quoteCoin"
                                }],
                            )?
                            .into(),
                            market: market.name().into(),
                        })
                    })
                    .collect::<BackendResult<Vec<_>>>()
                    .map(Value::Array)
            }
            "ticker" => {
                let row = if data.is_array() {
                    array(data)?
                        .iter()
                        .find(|r| r["symbol"].as_str() == symbol.as_deref())
                        .ok_or_else(|| error("invalid_response", "Requested ticker absent"))?
                } else {
                    data
                };
                let returned = text(&row["symbol"])?;
                if Some(returned) != symbol.as_deref() {
                    return Err(error("invalid_response", "Unexpected ticker symbol"));
                }
                let (last, bid, ask, volume) = if market == MarketKind::Spot {
                    ("lastPrice", "bidPrice", "askPrice", "volume")
                } else {
                    ("lastPrice", "bid1", "ask1", "volume24")
                };
                serialize(Ticker {
                    symbol: returned.into(),
                    last: number(&row[last])?,
                    bid: optional_number(&row[bid])?,
                    ask: optional_number(&row[ask])?,
                    volume: optional_number(&row[volume])?,
                })
            }
            "candles" => normalize_candles(data, market, limit),
            "order_book" => {
                let levels = |name: &str| -> BackendResult<Value> {
                    array(&data[name])?
                        .iter()
                        .take(limit)
                        .map(|row| {
                            let row = array(row)?;
                            if row.len() < 2 {
                                return Err(error(
                                    "invalid_response",
                                    "Malformed order book level",
                                ));
                            }
                            // Futures third column is order count, not quantity.
                            Ok(json!([number(&row[0])?, number(&row[1])?]))
                        })
                        .collect::<BackendResult<Vec<_>>>()
                        .map(Value::Array)
                };
                Ok(json!({"symbol":symbol,"bids":levels("bids")?,"asks":levels("asks")?}))
            }
            "trades" => array(data)?
                .iter()
                .take(limit)
                .map(|row| {
                    let (time, price, quantity, side) = if market == MarketKind::Spot {
                        let maker = row["isBuyerMaker"]
                            .as_bool()
                            .ok_or_else(|| error("invalid_response", "Missing trade maker flag"))?;
                        (
                            &row["time"],
                            &row["price"],
                            &row["qty"],
                            if maker { "sell" } else { "buy" },
                        )
                    } else {
                        let side = match row["T"].as_i64() {
                            Some(1) => "buy",
                            Some(2) => "sell",
                            _ => return Err(error("invalid_response", "Unknown trade side")),
                        };
                        (&row["t"], &row["p"], &row["v"], side)
                    };
                    Ok(json!({"time":integer(time)?/1000,"price":number(price)?,
                    "quantity":number(quantity)?,"side":side}))
                })
                .collect::<BackendResult<Vec<_>>>()
                .map(Value::Array),
            "funding" => Ok(json!({"symbol":symbol,
                    "rate":signed_number(&data["fundingRate"])?,
                    "next_time":integer(&data["nextSettleTime"])?/1000,
                    "interval_hours":number(&data["collectCycle"])?})),
            _ => unreachable!(),
        }
    }
}

fn signed_number(value: &Value) -> BackendResult<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|n| n.is_finite())
        .ok_or_else(|| error("invalid_response", "Invalid funding rate"))
}

fn normalize_candles(data: &Value, market: MarketKind, limit: usize) -> BackendResult<Value> {
    let mut candles = BTreeMap::new();
    let mut add =
        |time: i64, open: &Value, high: &Value, low: &Value, close: &Value, volume: &Value| {
            let candle = Candle {
                time,
                open: number(open)?,
                high: number(high)?,
                low: number(low)?,
                close: number(close)?,
                volume: number(volume)?,
            };
            if candle.low > candle.high
                || candle.open < candle.low
                || candle.open > candle.high
                || candle.close < candle.low
                || candle.close > candle.high
            {
                return Err(error("invalid_response", "Inconsistent candle OHLC"));
            }
            candles.insert(time, serialize(candle)?);
            Ok(())
        };
    if market == MarketKind::Spot {
        for row in array(data)? {
            let row = array(row)?;
            if row.len() < 6 {
                return Err(error("invalid_response", "Malformed candle row"));
            }
            add(
                integer(&row[0])? / 1000,
                &row[1],
                &row[2],
                &row[3],
                &row[4],
                &row[5],
            )?;
        }
    } else {
        let names = ["time", "open", "high", "low", "close", "vol"];
        let columns = names
            .iter()
            .map(|name| array(&data[*name]))
            .collect::<BackendResult<Vec<_>>>()?;
        if columns.iter().any(|c| c.len() != columns[0].len()) {
            return Err(error("invalid_response", "Mismatched candle columns"));
        }
        for (i, time) in columns[0].iter().enumerate() {
            add(
                integer(time)?,
                &columns[1][i],
                &columns[2][i],
                &columns[3][i],
                &columns[4][i],
                &columns[5][i],
            )?;
        }
    }
    let skip = candles.len().saturating_sub(limit);
    Ok(Value::Array(candles.into_values().skip(skip).collect()))
}
