//! Connector contracts, not UI port types. Conditional provider rules remain in execute.
use serde_json::{Value, json};
use tradeslope_sdk::{Field, Schema, candle_schema, market_schema, ticker_schema};

fn field(schema: Schema, required: bool, description: &str, default: Option<Value>) -> Field {
    Field {
        schema,
        required,
        description: description.into(),
        default,
    }
}

fn object(fields: Vec<(&str, Field)>) -> Schema {
    Schema::Object {
        fields: fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
    }
}

fn array(items: Schema) -> Schema {
    Schema::Array {
        items: Box::new(items),
    }
}

fn choices(values: &[&str]) -> Schema {
    Schema::Enum {
        values: values.iter().map(|v| (*v).into()).collect(),
    }
}

fn union(variants: Vec<Schema>) -> Schema {
    Schema::Union { variants }
}

fn decimal() -> Schema {
    union(vec![Schema::String, Schema::Number])
}

fn required(schema: Schema, description: &str) -> Field {
    field(schema, true, description, None)
}

pub(crate) fn sources() -> Vec<tradeslope_sdk::DataSource> {
    use tradeslope_sdk::{DataSource, SourceCatalog};
    ["spot", "futures"]
        .into_iter()
        .flat_map(|market| {
            ["candles", "ticker"]
                .into_iter()
                .map(move |operation| DataSource {
                    id: format!("{market}_{operation}"),
                    title: if market == "spot" { "Spot" } else { "Futures" }.into(),
                    operation: operation.into(),
                    params: json!({"market": market}),
                    configuration: object(vec![(
                        "symbol",
                        field(
                            Schema::String,
                            true,
                            "Trading pair",
                            Some(json!("BTC_USDT")),
                        ),
                    )]),
                    controls: if operation == "candles" {
                        ["interval", "end", "limit"]
                            .map(|key| (key.into(), key.into()))
                            .into()
                    } else {
                        Default::default()
                    },
                    catalog: Some(SourceCatalog {
                        operation: "markets".into(),
                        params: json!({"market": market}),
                        parameter: "symbol".into(),
                        value_field: "symbol".into(),
                        label_field: "symbol".into(),
                    }),
                })
        })
        .collect()
}

pub(crate) fn parameters(op: &str) -> Schema {
    let mut fields = vec![(
        "market",
        field(
            choices(&["spot", "futures"]),
            true,
            "Market; required explicitly. Funding and positions support futures only. Default is a suggested form value, not an execution fallback.",
            Some(json!("spot")),
        ),
    )];
    if !matches!(op, "health" | "markets" | "balances") {
        fields.push(("symbol", field(
            Schema::String,
            !matches!(op, "positions" | "cancel_order"),
            "Compact or BASE_QUOTE alphanumeric pair (3..48 chars, case insensitive). Required except positions (optional futures filter) and cancel_order (required on spot, forbidden on futures). Suggested default only; no execution fallback.",
            Some(json!("BTC_USDT")),
        )));
    }
    if op == "candles" {
        fields.push(("interval", field(
            choices(&["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1w", "1M"]), true,
            "Candle interval; 1w is futures-only. Required explicitly; suggested default is not an execution fallback.",
            Some(json!("1h")),
        )));
    }
    if matches!(op, "candles" | "order_history" | "user_trades") {
        fields.push(("start", field(Schema::Integer, false,
            "Inclusive UTC Unix seconds, 0..253402300799; must not exceed end. Absent means provider-selected range. Spot order history max 7 days; futures private history max 90 days.",
            None)));
        fields.push(("end", field(Schema::Integer, false,
            "UTC Unix seconds, 0..253402300799; must not precede start. Absent means provider-selected range.",
            None)));
    }
    if matches!(
        op,
        "candles" | "order_book" | "trades" | "open_orders" | "order_history" | "user_trades"
    ) {
        fields.push(("limit", field(Schema::Integer, false,
            "Requested count, 1..1000; defaults to 200 before clamping. Futures trades/private pages clamp to 100. open_orders accepts this only on futures. Futures candles trim the provider window locally.",
            Some(json!(200)))));
    }
    if matches!(op, "open_orders" | "order_history" | "user_trades") {
        fields.push((
            "page",
            field(
                Schema::Integer,
                false,
                "Futures-only page number, 1..100000; defaults to 1. Inapplicable on spot.",
                Some(json!(1)),
            ),
        ));
    }
    if matches!(op, "cancel_order" | "user_trades") {
        fields.push(("order_id", field(Schema::String, false,
            "cancel_order: required positive u64 string (max 20 chars) on futures; on spot supply exactly one of order_id (max 64) or client_order_id. user_trades: optional spot-only order filter (max 64). ASCII letters/digits/underscore/hyphen only.",
            None)));
    }
    if matches!(op, "place_order" | "cancel_order") {
        fields.push(("client_order_id", field(Schema::String, false,
            "ASCII alphanumeric/underscore/hyphen identifier, max 32 chars. Optional on placement; on spot cancellation supply exactly one of this or order_id; forbidden on futures cancellation.",
            None)));
    }
    if op == "place_order" {
        fields.push(("side", required(union(vec![choices(&["BUY", "SELL"]), Schema::Integer]),
            "Spot string enum BUY or SELL. Futures integer: 1 open long, 2 close short, 3 open short, 4 close long. Explicitly required; execute validates the market-specific union.")));
        fields.push(("type", required(union(vec![choices(&[
            "LIMIT", "MARKET", "LIMIT_MAKER", "IMMEDIATE_OR_CANCEL", "FILL_OR_KILL",
        ]), Schema::Integer]),
            "Spot string enum LIMIT, MARKET, LIMIT_MAKER, IMMEDIATE_OR_CANCEL, FILL_OR_KILL. Futures integer: 1 limit, 2 post-only, 3 IOC, 4 FOK, 5 market, 6 market-to-limit. Explicitly required.")));
        fields.push(("quantity", field(decimal(), false,
            "Positive plain decimal string or JSON number, max 64 chars; prefer string for precision. Required for futures (contracts), spot non-market orders and MARKET SELL; forbidden for spot MARKET BUY.",
            None)));
        fields.push(("quote_quantity", field(decimal(), false,
            "Positive plain decimal string or JSON number, max 64 chars. Required only for spot MARKET BUY; forbidden for all other orders.",
            None)));
        fields.push(("price", field(decimal(), false,
            "Positive plain decimal string or JSON number, max 64 chars. Required for non-market orders; forbidden for spot MARKET and futures type 5 (connector supplies native zero). No exponent/sign/whitespace; futures rejects leading zeros.",
            None)));
        fields.push(("open_type", field(Schema::Integer, false,
            "Required on futures: 1 isolated, 2 cross. Forbidden on spot; no inferred margin mode.",
            None)));
        fields.push(("leverage", field(Schema::Integer, false,
            "Required on futures, integer 1..500 (exchange/account may impose lower bounds). Forbidden on spot; no inferred leverage.",
            None)));
        fields.push((
            "position_id",
            field(
                Schema::Integer,
                false,
                "Optional futures position ID, positive i64; forbidden on spot.",
                None,
            ),
        ));
        fields.push((
            "position_mode",
            field(
                Schema::Integer,
                false,
                "Optional futures mode: 1 hedge, 2 one-way; forbidden on spot.",
                None,
            ),
        ));
    }
    object(fields)
}

pub(crate) fn output(op: &str) -> Schema {
    match op {
        "markets" => market_schema(),
        "ticker" => ticker_schema(),
        "candles" => candle_schema(),
        "health" => object(vec![
            (
                "ok",
                required(Schema::Boolean, "True after a successful provider ping."),
            ),
            (
                "market",
                required(choices(&["spot", "futures"]), "Selected market."),
            ),
        ]),
        "order_book" => {
            let levels = || {
                required(
                    array(Schema::Tuple {
                        items: vec![Schema::Number, Schema::Number],
                    }),
                    "Provider-ordered levels, each exactly [price, quantity]. Futures quantity is contracts; provider order-count column is omitted.",
                )
            };
            object(vec![
                (
                    "symbol",
                    required(Schema::String, "Normalized provider symbol."),
                ),
                ("bids", levels()),
                ("asks", levels()),
            ])
        }
        "trades" => array(object(vec![
            (
                "time",
                required(
                    Schema::Integer,
                    "UTC Unix seconds, truncated from provider milliseconds.",
                ),
            ),
            (
                "price",
                required(Schema::Number, "Finite nonnegative trade price."),
            ),
            (
                "quantity",
                required(
                    Schema::Number,
                    "Base quantity on spot; contracts on futures.",
                ),
            ),
            (
                "side",
                required(choices(&["buy", "sell"]), "Aggressor side."),
            ),
        ])),
        "funding" => object(vec![
            (
                "symbol",
                required(Schema::String, "Normalized futures symbol."),
            ),
            (
                "rate",
                required(
                    Schema::Number,
                    "Finite signed funding rate; negative values allowed.",
                ),
            ),
            (
                "next_time",
                required(Schema::Integer, "Next settlement UTC Unix seconds."),
            ),
            (
                "interval_hours",
                required(Schema::Number, "Funding collection interval in hours."),
            ),
        ]),
        // Signed endpoints intentionally preserve native provider data and success envelopes.
        _ => Schema::Any,
    }
}
