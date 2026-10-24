use crate::{
    Ourbit, error,
    params::{MarketKind, Params},
};
use reqwest::Method;
use serde_json::{Map, Value};
use tradeslope_sdk::BackendResult;

type Fields = Vec<(String, String)>;

fn add(fields: &mut Fields, key: &str, value: Option<String>) {
    if let Some(value) = value {
        fields.push((key.into(), value));
    }
}

fn id(
    params: &mut Params,
    name: &str,
    required: bool,
    max: usize,
) -> BackendResult<Option<String>> {
    let value = if required {
        Some(params.required_string(name)?)
    } else {
        params.string(name)?
    };
    if value.as_ref().is_some_and(|s| {
        s.len() > max
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }) {
        return Err(error(
            "invalid_params",
            &format!("{name} must be a short alphanumeric identifier"),
        ));
    }
    Ok(value)
}

fn choice(params: &mut Params, name: &str, choices: &[&str]) -> BackendResult<String> {
    let value = params.required_string(name)?;
    if !choices.contains(&value.as_str()) {
        return Err(error("invalid_params", &format!("Unsupported {name}")));
    }
    Ok(value)
}

impl Ourbit {
    pub(crate) fn private(&self, op: &str, market: MarketKind, p: Params) -> BackendResult<Value> {
        if market == MarketKind::Spot {
            self.spot_private(op, p)
        } else {
            self.futures_private(op, p)
        }
    }

    fn spot_private(&self, op: &str, mut p: Params) -> BackendResult<Value> {
        if op == "positions" {
            return Err(error(
                "unsupported_capability",
                "Spot does not have contract positions",
            ));
        }
        let mut fields = Vec::new();
        if op != "balances" {
            add(&mut fields, "symbol", p.symbol(MarketKind::Spot, true)?);
        }
        let (method, path) = match op {
            "balances" => (Method::GET, "/api/v3/account"),
            "open_orders" => (Method::GET, "/api/v3/openOrders"),
            "order_history" | "user_trades" => {
                let (start, end) = p.times()?;
                if op == "order_history"
                    && matches!((start,end), (Some(a),Some(b)) if b-a > 7*86400)
                {
                    return Err(error(
                        "invalid_params",
                        "Spot order history window cannot exceed seven days",
                    ));
                }
                add(
                    &mut fields,
                    "startTime",
                    start.map(|n| (n * 1000).to_string()),
                );
                add(&mut fields, "endTime", end.map(|n| (n * 1000).to_string()));
                fields.push(("limit".into(), p.limit(1000)?.to_string()));
                if op == "user_trades" {
                    add(&mut fields, "orderId", id(&mut p, "order_id", false, 64)?);
                }
                (
                    Method::GET,
                    if op == "order_history" {
                        "/api/v3/allOrders"
                    } else {
                        "/api/v3/myTrades"
                    },
                )
            }
            "place_order" => {
                let side = choice(&mut p, "side", &["BUY", "SELL"])?;
                let order_type = choice(
                    &mut p,
                    "type",
                    &[
                        "LIMIT",
                        "MARKET",
                        "LIMIT_MAKER",
                        "IMMEDIATE_OR_CANCEL",
                        "FILL_OR_KILL",
                    ],
                )?;
                let quantity = p.decimal("quantity", false)?;
                let quote_quantity = p.decimal("quote_quantity", false)?;
                let price = p.decimal("price", false)?;
                if order_type == "MARKET" {
                    let valid = price.is_none()
                        && if side == "BUY" {
                            quantity.is_none() && quote_quantity.is_some()
                        } else {
                            quantity.is_some() && quote_quantity.is_none()
                        };
                    if !valid {
                        return Err(error(
                            "invalid_params",
                            "MARKET BUY requires only quote_quantity; MARKET SELL only quantity; neither accepts price",
                        ));
                    }
                } else if price.is_none() || quantity.is_none() || quote_quantity.is_some() {
                    return Err(error(
                        "invalid_params",
                        "Limit orders require price and quantity; quote_quantity is inapplicable",
                    ));
                }
                fields.push(("side".into(), side));
                fields.push(("type".into(), order_type));
                add(&mut fields, "quantity", quantity);
                add(&mut fields, "quoteOrderQty", quote_quantity);
                add(&mut fields, "price", price);
                add(
                    &mut fields,
                    "newClientOrderId",
                    id(&mut p, "client_order_id", false, 32)?,
                );
                (Method::POST, "/api/v3/order")
            }
            "cancel_order" => {
                let order_id = id(&mut p, "order_id", false, 64)?;
                let client_id = id(&mut p, "client_order_id", false, 32)?;
                if order_id.is_some() == client_id.is_some() {
                    return Err(error(
                        "invalid_params",
                        "Supply exactly one of order_id or client_order_id",
                    ));
                }
                add(&mut fields, "orderId", order_id);
                add(&mut fields, "origClientOrderId", client_id);
                (Method::DELETE, "/api/v3/order")
            }
            "cancel_all" => (Method::DELETE, "/api/v3/openOrders"),
            _ => return Err(error("unsupported_operation", "Unknown private operation")),
        };
        p.finish()?;
        self.http.request(
            MarketKind::Spot,
            method,
            path,
            fields,
            None,
            Some(&self.credentials(MarketKind::Spot)?),
        )
    }

    fn futures_private(&self, op: &str, mut p: Params) -> BackendResult<Value> {
        let mut fields = Vec::new();
        let mut body = Map::new();
        let path: String;
        let method;
        match op {
            "balances" => {
                path = "/api/v1/private/account/assets".into();
                method = Method::GET;
            }
            "positions" => {
                add(&mut fields, "symbol", p.symbol(MarketKind::Futures, false)?);
                path = "/api/v1/private/position/open_positions".into();
                method = Method::GET;
            }
            "open_orders" => {
                let symbol = p.symbol(MarketKind::Futures, true)?.unwrap();
                fields.push((
                    "page_num".into(),
                    p.integer("page", 1, 100000)?.unwrap_or(1).to_string(),
                ));
                fields.push(("page_size".into(), p.limit(100)?.to_string()));
                path = format!("/api/v1/private/order/list/open_orders/{symbol}");
                method = Method::GET;
            }
            "order_history" | "user_trades" => {
                add(&mut fields, "symbol", p.symbol(MarketKind::Futures, true)?);
                let (start, end) = p.times()?;
                if matches!((start, end), (Some(a), Some(b)) if b-a > 90*86400) {
                    return Err(error(
                        "invalid_params",
                        "Futures history window cannot exceed 90 days",
                    ));
                }
                add(
                    &mut fields,
                    "start_time",
                    start.map(|n| (n * 1000).to_string()),
                );
                add(&mut fields, "end_time", end.map(|n| (n * 1000).to_string()));
                fields.push((
                    "page_num".into(),
                    p.integer("page", 1, 100000)?.unwrap_or(1).to_string(),
                ));
                fields.push(("page_size".into(), p.limit(100)?.to_string()));
                path = if op == "order_history" {
                    "/api/v1/private/order/list/history_orders"
                } else {
                    "/api/v1/private/order/list/order_deals"
                }
                .into();
                method = Method::GET;
            }
            "place_order" => {
                body.insert(
                    "symbol".into(),
                    Value::String(p.symbol(MarketKind::Futures, true)?.unwrap()),
                );
                // Native numeric enums are explicit: opening/closing is not inferred from BUY/SELL.
                body.insert("side".into(), p.required_integer("side", 1, 4)?.into());
                body.insert("type".into(), p.required_integer("type", 1, 6)?.into());
                body.insert(
                    "openType".into(),
                    p.required_integer("open_type", 1, 2)?.into(),
                );
                body.insert(
                    "leverage".into(),
                    p.required_integer("leverage", 1, 500)?.into(),
                );
                insert_decimal(&mut body, "vol", p.decimal("quantity", true)?.unwrap())?;
                // Price is required by the documented request, including explicitly supplied 0
                // for a native market order; handled below rather than silently defaulted.
                let price = p.decimal("price", false)?;
                let order_type = body["type"].as_i64().unwrap();
                if order_type == 5 {
                    if price.is_some() {
                        return Err(error(
                            "invalid_params",
                            "Futures market orders do not accept price",
                        ));
                    }
                    body.insert("price".into(), 0.into());
                } else {
                    insert_decimal(
                        &mut body,
                        "price",
                        price.ok_or_else(|| {
                            error("invalid_params", "price is required for non-market orders")
                        })?,
                    )?;
                }
                if let Some(position_id) = p.integer("position_id", 1, i64::MAX)? {
                    body.insert("positionId".into(), position_id.into());
                }
                if let Some(mode) = p.integer("position_mode", 1, 2)? {
                    body.insert("positionMode".into(), mode.into());
                }
                if let Some(external_oid) = id(&mut p, "client_order_id", false, 32)? {
                    body.insert("externalOid".into(), external_oid.into());
                }
                path = "/api/v1/private/order/submit".into();
                method = Method::POST;
            }
            "cancel_order" => {
                // Futures cancellation endpoint requires an array of numeric exchange IDs.
                let order_id = id(&mut p, "order_id", true, 20)?.unwrap();
                let order_id: u64 = order_id.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                    error(
                        "invalid_params",
                        "Futures order_id must be a positive u64 string",
                    )
                })?;
                p.finish()?;
                return self.http.request(
                    MarketKind::Futures,
                    Method::POST,
                    "/api/v1/private/order/cancel",
                    fields,
                    Some(Value::Array(vec![order_id.into()])),
                    Some(&self.credentials(MarketKind::Futures)?),
                );
            }
            "cancel_all" => {
                // Require a symbol deliberately: never cancel an entire account implicitly.
                body.insert(
                    "symbol".into(),
                    p.symbol(MarketKind::Futures, true)?.unwrap().into(),
                );
                path = "/api/v1/private/order/cancel_all".into();
                method = Method::POST;
            }
            _ => return Err(error("unsupported_operation", "Unknown futures operation")),
        }
        p.finish()?;
        let body = if method == Method::POST {
            Some(Value::Object(body))
        } else {
            None
        };
        self.http.request(
            MarketKind::Futures,
            method,
            &path,
            fields,
            body,
            Some(&self.credentials(MarketKind::Futures)?),
        )
    }
}

fn insert_decimal(body: &mut Map<String, Value>, key: &str, decimal: String) -> BackendResult<()> {
    // arbitrary_precision preserves the exact decimal text in the signed JSON, unlike f64.
    let value: Value = serde_json::from_str(&decimal).map_err(|_| {
        error(
            "invalid_params",
            "Decimal must also be a valid JSON number (no leading zeros)",
        )
    })?;
    if !value.is_number() {
        return Err(error("invalid_params", "Expected decimal number"));
    }
    body.insert(key.into(), value);
    Ok(())
}
