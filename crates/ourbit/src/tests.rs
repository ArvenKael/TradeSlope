use super::*;
use crate::transport::{MAX_BYTES, sign};
use serde_json::json;
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    url: String,
    requests: mpsc::Receiver<(Instant, String)>,
    worker: thread::JoinHandle<()>,
}

#[test]
fn declarative_sources_are_ready_typed_datasets() {
    use tradeslope_sdk::{
        BackendHost, Field, InputPort, OperationKind, Permissions, Schema, source_binding,
    };
    let backend =
        Ourbit::with_endpoints("http://127.0.0.1:1", "http://127.0.0.1:1", None, None).unwrap();
    let host = BackendHost::new(vec![std::sync::Arc::new(backend)]).unwrap();
    let client = host.client(Permissions::default());
    let sources = client.sources();
    assert_eq!(sources.len(), 4);
    for descriptor in sources {
        let source = descriptor.source;
        let candles = source.operation == "candles";
        assert_eq!(descriptor.backend, "ourbit");
        assert_eq!(
            source.title,
            if source.params["market"] == "spot" {
                "Spot"
            } else {
                "Futures"
            }
        );
        let catalog = source.catalog.as_ref().unwrap();
        assert_eq!(catalog.operation, "markets");
        assert_eq!(catalog.parameter, "symbol");
        assert_eq!(catalog.params["market"], source.params["market"]);
        assert_eq!(catalog.value_field, "symbol");
        let port = InputPort {
            id: "data".into(),
            title: "Data".into(),
            kind: OperationKind::Query,
            parameters: Schema::object(if candles {
                [(
                    "interval".into(),
                    Field {
                        schema: Schema::Enum {
                            values: vec!["1h".into(), "1d".into()],
                        },
                        required: true,
                        default: None,
                        description: String::new(),
                    },
                )]
                .into()
            } else {
                Default::default()
            }),
            output: schema::output(&source.operation),
            required: true,
        };
        for symbol in ["BTC_USDT", "ETHUSDT", "ETH_USDT"] {
            let binding = source_binding(
                &client,
                &port,
                "ourbit",
                &source.id,
                json!({"symbol":symbol}),
            )
            .unwrap();
            assert_eq!(binding.params["symbol"], symbol);
            assert!(binding.params.get("interval").is_none());
            assert_eq!(
                binding.parameter_fields.get("interval").map(String::as_str),
                candles.then_some("interval")
            );
            assert!(binding.params.get("limit").is_none());
            assert_eq!(binding.parameter_fields.len(), usize::from(candles));
        }
        assert!(
            source_binding(
                &client,
                &port,
                "ourbit",
                &source.id,
                json!({"symbol":"BTC_USDT","interval":"1m"})
            )
            .is_err()
        );
        if candles {
            let mut history = port.clone();
            let Schema::Object { fields } = &mut history.parameters else {
                unreachable!()
            };
            for key in ["end", "limit"] {
                fields.insert(
                    key.into(),
                    Field {
                        schema: Schema::Integer,
                        required: false,
                        default: None,
                        description: String::new(),
                    },
                );
            }
            let binding =
                source_binding(&client, &history, "ourbit", &source.id, json!({})).unwrap();
            assert_eq!(binding.parameter_fields, source.controls);
            assert!(binding.params.get("end").is_none());
            assert!(binding.params.get("limit").is_none());
        }
    }
}

fn reply(status: u16, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    )
}

impl Fixture {
    fn new(replies: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, requests) = mpsc::channel();
        let worker = thread::spawn(move || {
            for response in replies {
                let deadline = Instant::now() + Duration::from_secs(30);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "Fixture did not receive request");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(e) => panic!("fixture accept: {e}"),
                    }
                };
                let arrived = Instant::now();
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                tx.send((arrived, read_request(&mut stream))).unwrap();
                // The size-bound client may close before the body is fully sent.
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            url,
            requests,
            worker,
        }
    }

    fn json(value: Value) -> Self {
        Self::new(vec![reply(200, "", &value.to_string())])
    }

    fn backend(&self, credentials: bool) -> Ourbit {
        Ourbit::with_endpoints(
            &self.url,
            &self.url,
            credentials.then(|| Credentials::new("fixture-key", "fixture-secret").unwrap()),
            None,
        )
        .unwrap()
    }

    fn request(&self) -> String {
        self.requests
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .1
    }

    fn finish(self) {
        self.worker.join().unwrap();
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "Request ended prematurely");
        data.extend_from_slice(&buf[..n]);
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&data[..end]);
            let length: usize = header
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            if data.len() >= end + 4 + length {
                return String::from_utf8(data).unwrap();
            }
        }
    }
}

fn headers(request: &str) -> HashMap<String, String> {
    request
        .split("\r\n\r\n")
        .next()
        .unwrap()
        .lines()
        .skip(1)
        .map(|line| {
            let (k, v) = line.split_once(':').unwrap();
            (k.to_ascii_lowercase(), v.trim().into())
        })
        .collect()
}

fn target(request: &str) -> &str {
    request
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
}

fn query(request: &str) -> HashMap<String, String> {
    let raw = target(request)
        .split_once('?')
        .map_or("", |(_, query)| query);
    url::form_urlencoded::parse(raw.as_bytes())
        .into_owned()
        .collect()
}

/// Exercise host-side parameter/output validation, not just connector normalization.
fn checked_call(backend: Ourbit, operation: &str, params: Value) -> BackendResult<Value> {
    use std::sync::Arc;
    use tradeslope_sdk::{BackendHost, Permissions};
    BackendHost::new(vec![Arc::new(backend)])?
        .client(Permissions {
            private_read: true,
            trade: true,
        })
        .call("ourbit", operation, params)
}

#[test]
fn deterministic_signatures_match_independent_vectors() {
    // Official spot v3 example 2; examples 1 and 2 publish inconsistent hashes.
    assert_eq!(
        sign(
            "45d0b3c26f2644f19bfb98b07741b2f5",
            "symbol=BTCUSDT&side=BUY&type=LIMIT&quantity=1&price=11&recvWindow=5000&timestamp=1644489390087"
        ),
        "fd3e4e8543c5188531eb7279d68ae7d26a573d0fc5ab0d18eb692451654d837a"
    );
    assert_eq!(
        sign(
            "fixture-secret",
            "fixture-key1700000000000page_num=1&page_size=100&symbol=BTC_USDT"
        ),
        "0189749f1d2852a56ac17ac1b877eec9e72bd71e1352a76a1bf895007ab026f5"
    );
    assert_eq!(
        sign(
            "fixture-secret",
            "fixture-key1700000000000{\"price\":1.234567890123456789,\"symbol\":\"BTC_USDT\",\"vol\":2}"
        ),
        "2a6b6d69b3c5c33df3fa3d76f519143efa35f3b2877b39e7026f2288521f7cdb"
    );
}

#[test]
fn manifest_has_exact_access_classes_and_backend_is_thread_safe() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Ourbit>();
    let backend = Ourbit::with_endpoints(SPOT_ENDPOINT, FUTURES_ENDPOINT, None, None).unwrap();
    let manifest = backend.manifest();
    assert_eq!(manifest.id, "ourbit");
    assert_eq!(
        manifest.operations.len(),
        PUBLIC.len() + PRIVATE.len() + TRADE.len()
    );
    for operation in manifest.operations {
        let expected = if PUBLIC.contains(&operation.id.as_str()) {
            Access::Public
        } else if PRIVATE.contains(&operation.id.as_str()) {
            Access::PrivateRead
        } else {
            Access::Trade
        };
        assert_eq!(operation.access, expected);
        assert_eq!(
            operation.kind,
            if expected == Access::Trade {
                tradeslope_sdk::OperationKind::Action
            } else {
                tradeslope_sdk::OperationKind::Query
            }
        );
    }
}

#[test]
fn manifest_declares_every_parameter_with_descriptions_and_truthful_defaults() {
    use tradeslope_sdk::Schema;
    let backend = Ourbit::with_endpoints(SPOT_ENDPOINT, FUTURES_ENDPOINT, None, None).unwrap();
    let manifest = backend.manifest();
    for (id, names) in [
        ("health", "market"),
        ("markets", "market"),
        ("ticker", "market symbol"),
        ("candles", "market symbol interval start end limit"),
        ("order_book", "market symbol limit"),
        ("trades", "market symbol limit"),
        ("funding", "market symbol"),
        ("balances", "market"),
        ("positions", "market symbol"),
        ("open_orders", "market symbol page limit"),
        ("order_history", "market symbol start end page limit"),
        ("user_trades", "market symbol order_id start end page limit"),
        (
            "place_order",
            "market symbol side type quantity quote_quantity price client_order_id open_type leverage position_id position_mode",
        ),
        ("cancel_order", "market symbol order_id client_order_id"),
        ("cancel_all", "market symbol"),
    ] {
        let op = manifest.operations.iter().find(|op| op.id == id).unwrap();
        let Schema::Object { fields } = &op.parameters else {
            panic!("{id}: parameters must be a closed object");
        };
        let mut expected: Vec<_> = names.split_whitespace().collect();
        expected.sort();
        assert_eq!(
            fields.keys().map(String::as_str).collect::<Vec<_>>(),
            expected,
            "{id}"
        );
        assert!(fields.values().all(|f| !f.description.is_empty()), "{id}");
        assert!(fields["market"].required);
        assert_eq!(fields["market"].default, Some(json!("spot")));
        assert!(
            matches!(&fields["market"].schema, Schema::Enum { values } if values == &["spot", "futures"])
        );
        if let Some(f) = fields.get("limit") {
            assert!(!f.required);
            assert_eq!(f.default, Some(json!(200)));
        }
        if let Some(f) = fields.get("page") {
            assert!(!f.required);
            assert_eq!(f.default, Some(json!(1)));
        }
        if PUBLIC.contains(&id) {
            assert!(!matches!(op.output, Schema::Any));
        } else {
            assert!(matches!(op.output, Schema::Any));
        }
    }
    let candles = manifest
        .operations
        .iter()
        .find(|op| op.id == "candles")
        .unwrap();
    let Schema::Object { fields } = &candles.parameters else {
        unreachable!()
    };
    assert!(fields["symbol"].required);
    assert_eq!(fields["symbol"].default, Some(json!("BTC_USDT")));
    assert!(fields["interval"].required);
    assert_eq!(fields["interval"].default, Some(json!("1h")));
    assert!(matches!(&fields["interval"].schema, Schema::Enum { values }
        if values == &["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1w", "1M"]));
}

#[test]
fn sdk_clients_enforce_manifest_permissions_before_network() {
    use std::sync::Arc;
    use tradeslope_sdk::{BackendHost, Permissions};
    let backend =
        Ourbit::with_endpoints("http://127.0.0.1:1", "http://127.0.0.1:1", None, None).unwrap();
    let host = BackendHost::new(vec![Arc::new(backend)]).unwrap();
    let public = host.client(Permissions::default());
    for (op, params) in [
        ("balances", json!({"market":"spot"})),
        ("place_order", json!({"market":"spot"})),
        ("cancel_order", json!({"market":"futures"})),
        ("cancel_all", json!({"market":"spot"})),
    ] {
        assert_eq!(
            public.call("ourbit", op, params).unwrap_err().code,
            "permission_denied"
        );
    }
    let private = host.client(Permissions {
        private_read: true,
        trade: false,
    });
    assert_eq!(
        private
            .call("ourbit", "balances", json!({"market":"spot"}))
            .unwrap_err()
            .code,
        "credentials_required"
    );
}

#[test]
fn invalid_params_fail_before_network_and_never_echo_input() {
    let backend =
        Ourbit::with_endpoints("http://127.0.0.1:1", "http://127.0.0.1:1", None, None).unwrap();
    for (op, params, code) in [
        ("ticker", json!({}), "invalid_params"),
        (
            "ticker",
            json!({"market":"SPOT","symbol":"BTCUSDT"}),
            "invalid_params",
        ),
        ("ticker", json!({"market":"spot"}), "invalid_params"),
        (
            "ticker",
            json!({"market":"spot","symbol":"../../fixture-secret"}),
            "invalid_params",
        ),
        (
            "ticker",
            json!({"market":"spot","symbol":"BTCUSDT","unknown":"fixture-secret"}),
            "invalid_params",
        ),
        (
            "candles",
            json!({"market":"spot","symbol":"BTCUSDT","interval":"1m","limit":1001}),
            "invalid_params",
        ),
        (
            "candles",
            json!({"market":"spot","symbol":"BTCUSDT","interval":"1m","limit":0}),
            "invalid_params",
        ),
        (
            "candles",
            json!({"market":"spot","symbol":"BTCUSDT","interval":"1m","limit":2.5}),
            "invalid_params",
        ),
        (
            "candles",
            json!({"market":"spot","symbol":"BTCUSDT","interval":"1m","start":2,"end":1}),
            "invalid_params",
        ),
        (
            "candles",
            json!({"market":"spot","symbol":"BTCUSDT","interval":"1m","start":-1}),
            "invalid_params",
        ),
        (
            "candles",
            json!({"market":"spot","symbol":"BTCUSDT","interval":"1w"}),
            "unsupported_capability",
        ),
        (
            "candles",
            json!({"market":"spot","symbol":"BTCUSDT","interval":"3m"}),
            "invalid_params",
        ),
        (
            "funding",
            json!({"market":"spot","symbol":"BTCUSDT"}),
            "unsupported_capability",
        ),
        (
            "positions",
            json!({"market":"spot"}),
            "unsupported_capability",
        ),
        ("balances", json!({"market":"spot"}), "credentials_required"),
        ("nope", json!({}), "unsupported_operation"),
        ("health", Value::Null, "invalid_params"),
    ] {
        let e = backend.execute(op, params).unwrap_err();
        assert_eq!(e.code, code, "{op}: {e}");
        assert!(!e.to_string().contains("fixture-secret"));
    }
}

#[test]
fn symbol_interval_time_mapping_and_candle_sort_dedupe() {
    let fixture = Fixture::json(json!([
        [
            1700000060000_i64,
            "2",
            "3",
            "1",
            "2.5",
            "0.000000000123456789"
        ],
        [1700000000000_i64, 1, 2, 0.5, 1.5, 7],
        [1700000060000_i64, "2", "4", "1", "3", "8"]
    ]));
    let result = checked_call(fixture.backend(true), "candles", json!({
        "market":"spot","symbol":"btc_usdt","interval":"1h","start":1700000000,"end":1700000120,"limit":2
    })).unwrap();
    assert_eq!(result[0]["time"], 1700000000);
    assert_eq!(result[1]["time"], 1700000060);
    assert_eq!(result[1]["high"], 4.0);
    let request = fixture.request();
    let q = query(&request);
    assert_eq!(q["symbol"], "BTCUSDT");
    assert_eq!(q["interval"], "60m");
    assert_eq!(q["startTime"], "1700000000000");
    assert_eq!(q["endTime"], "1700000120000");
    assert!(
        !headers(&request).contains_key("x-ourbit-apikey"),
        "Public calls must not send credentials"
    );
    fixture.finish();
}

#[test]
fn history_end_only_requests_use_provider_units_and_spot_window() {
    for market in ["spot", "futures"] {
        let rows = if market == "spot" {
            json!([[1700000000000_i64, 1, 2, 0.5, 1.5, 7]])
        } else {
            json!({"success":true,"code":0,"data":{
                "time":[1700000000],"open":[1],"high":[2],"low":[0.5],
                "close":[1.5],"vol":[7]
            }})
        };
        let fixture = Fixture::json(rows);
        let result = checked_call(
            fixture.backend(false),
            "candles",
            json!({
                "market":market,"symbol":"BTC_USDT","interval":"1h",
                "end":1700003599,"limit":1000
            }),
        )
        .unwrap();
        assert_eq!(result[0]["time"], 1700000000);
        let request = fixture.request();
        let q = query(&request);
        if market == "spot" {
            assert_eq!(q["endTime"], "1700003599000");
            assert_eq!(q["limit"], "1000");
            assert_eq!(q["startTime"], "1696406400000");
        } else {
            assert_eq!(q["end"], "1700003599");
            assert!(!q.contains_key("limit"));
            assert!(!q.contains_key("start"));
        }
        fixture.finish();
    }
}

#[test]
fn spot_monthly_history_uses_calendar_boundaries_and_clamps_to_epoch() {
    for (end, limit, start) in [
        (1709251199_i64, 1, 1706745600_i64), // Feb 2024, leap year
        (1709251200, 1, 1709251200),         // exact March open is inclusive
        (1709251199, 3, 1701388800),         // crosses year into December
        (4107542399, 1, 4105123200),         // February 2100 is not leap
        (951868799, 1, 949363200),           // February 2000 is leap
        (0, 1000, 0),                        // never send negative provider time
        (253402300799, 1, 253399622400),     // December 9999
    ] {
        let fixture = Fixture::json(json!([]));
        checked_call(
            fixture.backend(false),
            "candles",
            json!({
                "market":"spot","symbol":"BTC_USDT","interval":"1M","end":end,"limit":limit
            }),
        )
        .unwrap();
        let request = fixture.request();
        let q = query(&request);
        assert_eq!(
            q["startTime"],
            (start * 1000).to_string(),
            "end={end}, limit={limit}"
        );
        assert_eq!(q["endTime"], (end * 1000).to_string());
        fixture.finish();
    }
}

#[test]
fn futures_columnar_candles_use_seconds_and_trim_latest() {
    let fixture = Fixture::json(json!({"success":true,"code":0,"data":{
        "time":[1700000060,1700000000],"open":["2",1],"high":[3,2],"low":[1,0.5],
        "close":[2.5,1.5],"vol":[12,7]
    }}));
    let result = checked_call(
        fixture.backend(false),
        "candles",
        json!({
            "market":"futures","symbol":"BTCUSDT","interval":"1w","limit":1
        }),
    )
    .unwrap();
    assert_eq!(result.as_array().unwrap().len(), 1);
    assert_eq!(result[0]["time"], 1700000060);
    let request = fixture.request();
    assert_eq!(
        target(&request),
        "/api/v1/contract/kline/BTC_USDT?interval=Week1"
    );
    fixture.finish();
}

#[test]
fn malformed_provider_data_is_rejected() {
    for (market, op, data) in [
        (
            "spot",
            "candles",
            json!([[1700000000000_i64, "NaN", 2, 0.5, 1.5, 1]]),
        ),
        (
            "spot",
            "candles",
            json!([[1700000000000_i64, 1, 0.1, 0.5, 1.5, 1]]),
        ),
        ("spot", "candles", json!([[1700000000000_i64, 1, 2]])),
        ("spot", "candles", json!([[1.5, 1, 2, 0.5, 1.5, 1]])),
        (
            "futures",
            "candles",
            json!({"time":[1],"open":[],"high":[2],"low":[1],"close":[1],"vol":[1]}),
        ),
        (
            "spot",
            "ticker",
            json!({"symbol":"BTCUSDT","lastPrice":"inf"}),
        ),
        ("spot", "ticker", json!({"symbol":"ETHUSDT","lastPrice":1})),
        ("spot", "order_book", json!({"bids":[[1]],"asks":[]})),
    ] {
        let payload = if market == "futures" {
            json!({"success":true,"code":0,"data":data})
        } else {
            data
        };
        let fixture = Fixture::json(payload);
        let mut params = json!({"market":market,"symbol":"BTCUSDT"});
        if op == "candles" {
            params["interval"] = "1m".into();
        }
        assert_eq!(
            fixture.backend(false).execute(op, params).unwrap_err().code,
            "invalid_response"
        );
        fixture.request();
        fixture.finish();
    }
}

#[test]
fn public_normalized_schemas_and_default_limits() {
    for (market, op, payload, expected, path) in [
        (
            "spot",
            "health",
            json!({}),
            json!({"ok":true,"market":"spot"}),
            "/api/v3/ping",
        ),
        (
            "futures",
            "health",
            json!(1700000000000_i64),
            json!({"ok":true,"market":"futures"}),
            "/api/v1/contract/ping",
        ),
        (
            "spot",
            "markets",
            json!({"symbols":[{"symbol":"BTCUSDT","baseAsset":"BTC","quoteAsset":"USDT"}]}),
            json!([{"symbol":"BTCUSDT","base":"BTC","quote":"USDT","market":"spot"}]),
            "/api/v3/exchangeInfo",
        ),
        (
            "futures",
            "markets",
            json!([{"symbol":"BTC_USDT","baseCoin":"BTC","quoteCoin":"USDT"}]),
            json!([{"symbol":"BTC_USDT","base":"BTC","quote":"USDT","market":"futures"}]),
            "/api/v1/contract/detail",
        ),
        (
            "spot",
            "ticker",
            json!({"symbol":"BTCUSDT","lastPrice":"1.25","bidPrice":"1.2","askPrice":null,"volume":"2"}),
            json!({"symbol":"BTCUSDT","last":1.25,"bid":1.2,"ask":null,"volume":2.0}),
            "/api/v3/ticker/24hr?symbol=BTCUSDT",
        ),
        (
            "futures",
            "ticker",
            json!({"symbol":"BTC_USDT","lastPrice":1.25,"bid1":1.2,"ask1":1.3,"volume24":2}),
            json!({"symbol":"BTC_USDT","last":1.25,"bid":1.2,"ask":1.3,"volume":2.0}),
            "/api/v1/contract/ticker?symbol=BTC_USDT",
        ),
        (
            "spot",
            "ticker",
            json!({"symbol":"BTCUSDT","lastPrice":"1.25"}),
            json!({"symbol":"BTCUSDT","last":1.25,"bid":null,"ask":null,"volume":null}),
            "/api/v3/ticker/24hr?symbol=BTCUSDT",
        ),
        (
            "spot",
            "order_book",
            json!({"bids":[["1.2","3"]],"asks":[[1.3,4]]}),
            json!({"symbol":"BTCUSDT","bids":[[1.2,3.0]],"asks":[[1.3,4.0]]}),
            "/api/v3/depth?symbol=BTCUSDT&limit=200",
        ),
        (
            "futures",
            "order_book",
            json!({"bids":[[1.2,3,99]],"asks":[[1.3,4,23]]}),
            json!({"symbol":"BTC_USDT","bids":[[1.2,3.0]],"asks":[[1.3,4.0]]}),
            "/api/v1/contract/depth/BTC_USDT?limit=200",
        ),
        (
            "spot",
            "trades",
            json!([{"price":"1.2","qty":"0.123456789012345678","time":1700000000123_i64,"isBuyerMaker":true}]),
            json!([{"time":1700000000,"price":1.2,"quantity":0.123_456_789_012_345_68_f64,"side":"sell"}]),
            "/api/v3/trades?symbol=BTCUSDT&limit=200",
        ),
        (
            "futures",
            "trades",
            json!([{"p":1.2,"v":3,"t":1700000000123_i64,"T":1}]),
            json!([{"time":1700000000,"price":1.2,"quantity":3.0,"side":"buy"}]),
            "/api/v1/contract/deals/BTC_USDT?limit=100",
        ),
        (
            "futures",
            "funding",
            json!({"fundingRate":"-0.001","nextSettleTime":1700000000123_i64,"collectCycle":8}),
            json!({"symbol":"BTC_USDT","rate":-0.001,"next_time":1700000000,"interval_hours":8.0}),
            "/api/v1/contract/funding_rate/BTC_USDT",
        ),
    ] {
        let response = if market == "futures" {
            json!({"success":true,"code":0,"data":payload})
        } else {
            payload
        };
        let fixture = Fixture::json(response);
        let params = if matches!(op, "markets" | "health") {
            json!({"market":market})
        } else {
            json!({"market":market,"symbol":"BTCUSDT"})
        };
        assert_eq!(
            checked_call(fixture.backend(false), op, params).unwrap(),
            expected
        );
        assert_eq!(target(&fixture.request()), path);
        fixture.finish();
    }
}

#[test]
fn provider_errors_even_on_http_200_are_redacted() {
    for value in [
        json!({"code":-2011,"msg":"fixture-key fixture-secret"}),
        json!({"success":false,"code":0,"message":"fixture-secret"}),
        json!({"code":"fixture-secret","msg":"fixture-key"}),
    ] {
        let fixture = Fixture::json(value);
        let e = fixture
            .backend(true)
            .execute("balances", json!({"market":"spot"}))
            .unwrap_err();
        assert_eq!(e.code, "provider_error");
        assert!(!format!("{e:?}").contains("fixture-"));
        fixture.request();
        fixture.finish();
    }
}

#[test]
fn private_spot_requests_sign_exact_wire_query_and_keep_precision() {
    let fixture = Fixture::json(json!({"orderId":"123"}));
    let backend = fixture.backend(true);
    let result = checked_call(fixture.backend(true), "place_order", json!({
        "market":"spot","symbol":"BTC_USDT","side":"BUY","type":"LIMIT",
        "quantity":"0.123456789012345678","price":"1.000000000000000001","client_order_id":"sdk-1"
    })).unwrap();
    assert_eq!(result["orderId"], "123");
    let request = fixture.request();
    assert!(request.starts_with("POST /api/v3/order?"));
    let h = headers(&request);
    assert_eq!(h["x-ourbit-apikey"], "fixture-key");
    assert_eq!(query(&request)["quantity"], "0.123456789012345678");
    let unsigned = target(&request)
        .split_once('?')
        .unwrap()
        .1
        .split_once("&signature=")
        .unwrap();
    assert_eq!(unsigned.1, sign("fixture-secret", unsigned.0));
    assert_eq!(query(&request)["recvWindow"], "5000");
    assert!(!request.contains("fixture-secret"));
    assert!(!format!("{backend:?}").contains("fixture-key"));
    fixture.finish();
}

#[test]
fn futures_post_signs_exact_json_body_without_decimal_rounding() {
    let fixture = Fixture::json(json!({"success":true,"code":0,"data":"123"}));
    checked_call(
        fixture.backend(true),
        "place_order",
        json!({
            "market":"futures","symbol":"btcusdt","side":1,"type":1,"open_type":1,
            "leverage":5,"position_mode":1,"quantity":"2","price":"1.234567890123456789"
        }),
    )
    .unwrap();
    let request = fixture.request();
    assert_eq!(target(&request), "/api/v1/private/order/submit");
    let h = headers(&request);
    assert_eq!(h["apikey"], "fixture-key");
    let body = request.split_once("\r\n\r\n").unwrap().1;
    assert!(body.contains("\"price\":1.234567890123456789"));
    assert!(body.contains("\"vol\":2"));
    assert_eq!(
        h["signature"],
        sign(
            "fixture-secret",
            &format!("fixture-key{}{body}", h["request-time"])
        )
    );
    fixture.finish();
}

#[test]
fn private_reads_cancellations_and_pagination_map_to_distinct_apis() {
    for (market, op, extra, method, path) in [
        ("spot", "balances", json!({}), "GET", "/api/v3/account"),
        (
            "spot",
            "open_orders",
            json!({"symbol":"BTCUSDT"}),
            "GET",
            "/api/v3/openOrders",
        ),
        (
            "spot",
            "order_history",
            json!({"symbol":"BTCUSDT","start":1,"end":2}),
            "GET",
            "/api/v3/allOrders",
        ),
        (
            "spot",
            "user_trades",
            json!({"symbol":"BTCUSDT","order_id":"abc"}),
            "GET",
            "/api/v3/myTrades",
        ),
        (
            "spot",
            "cancel_order",
            json!({"symbol":"BTCUSDT","client_order_id":"sdk-1"}),
            "DELETE",
            "/api/v3/order",
        ),
        (
            "spot",
            "cancel_all",
            json!({"symbol":"BTCUSDT"}),
            "DELETE",
            "/api/v3/openOrders",
        ),
        (
            "futures",
            "balances",
            json!({}),
            "GET",
            "/api/v1/private/account/assets",
        ),
        (
            "futures",
            "positions",
            json!({"symbol":"BTCUSDT"}),
            "GET",
            "/api/v1/private/position/open_positions",
        ),
        (
            "futures",
            "open_orders",
            json!({"symbol":"BTCUSDT","page":2,"limit":999}),
            "GET",
            "/api/v1/private/order/list/open_orders/BTC_USDT",
        ),
        (
            "futures",
            "order_history",
            json!({"symbol":"BTCUSDT","start":1,"end":2}),
            "GET",
            "/api/v1/private/order/list/history_orders",
        ),
        (
            "futures",
            "user_trades",
            json!({"symbol":"BTCUSDT"}),
            "GET",
            "/api/v1/private/order/list/order_deals",
        ),
        (
            "futures",
            "cancel_order",
            json!({"order_id":"18446744073709551615"}),
            "POST",
            "/api/v1/private/order/cancel",
        ),
        (
            "futures",
            "cancel_all",
            json!({"symbol":"BTCUSDT"}),
            "POST",
            "/api/v1/private/order/cancel_all",
        ),
    ] {
        let fixture = Fixture::json(json!({"success":true,"code":0,"data":[]}));
        let mut params = extra;
        params["market"] = market.into();
        checked_call(fixture.backend(true), op, params).unwrap();
        let request = fixture.request();
        assert!(request.starts_with(&format!("{method} ")));
        assert_eq!(target(&request).split('?').next().unwrap(), path);
        if market == "futures" && method == "GET" {
            let h = headers(&request);
            let raw_query = target(&request).split_once('?').map_or("", |(_, q)| q);
            assert_eq!(
                h["signature"],
                sign(
                    "fixture-secret",
                    &format!("fixture-key{}{raw_query}", h["request-time"])
                )
            );
            if op == "open_orders" {
                assert_eq!(query(&request)["page_size"], "100");
                assert_eq!(query(&request)["page_num"], "2");
            }
            if op == "order_history" {
                assert_eq!(query(&request)["start_time"], "1000");
            }
        }
        if market == "futures" && op == "cancel_order" {
            assert_eq!(
                request.split_once("\r\n\r\n").unwrap().1,
                "[18446744073709551615]"
            );
        }
        fixture.finish();
    }
}

#[test]
fn financial_inputs_are_explicit_and_market_specific() {
    let backend = Ourbit::with_endpoints(
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
        Some(Credentials::new("fixture-key", "fixture-secret").unwrap()),
        None,
    )
    .unwrap();
    let spot = json!({"market":"spot","symbol":"BTCUSDT","type":"LIMIT","side":"BUY","price":"1","quantity":"1"});
    for (key, value) in [
        ("side", json!("buy")),
        ("side", Value::Null),
        ("type", json!("STOP")),
        ("quantity", json!("NaN")),
        ("quantity", json!("1e500")),
        ("quantity", json!("-1")),
        ("quantity", json!("0")),
        ("price", Value::Null),
        ("leverage", json!(5)),
        ("quote_quantity", json!("5")),
        ("quantity", json!(" 1")),
        ("client_order_id", json!("a&b")),
    ] {
        let mut p = spot.clone();
        p[key] = value;
        assert_eq!(
            backend.execute("place_order", p).unwrap_err().code,
            "invalid_params"
        );
    }
    let future = json!({"market":"futures","symbol":"BTCUSDT","type":1,"side":1,
        "price":"1","quantity":"1","leverage":5,"open_type":1});
    for key in ["side", "type", "leverage", "open_type", "quantity", "price"] {
        let mut p = future.clone();
        p.as_object_mut().unwrap().remove(key);
        assert_eq!(
            backend.execute("place_order", p).unwrap_err().code,
            "invalid_params"
        );
    }
    for p in [
        json!({"market":"spot","symbol":"BTCUSDT","side":"BUY","type":"MARKET","quantity":"1"}),
        json!({"market":"spot","symbol":"BTCUSDT","side":"SELL","type":"MARKET","quote_quantity":"1"}),
        json!({"market":"spot","symbol":"BTCUSDT","order_id":"abc","client_order_id":"sdk"}),
    ] {
        let op = if p.get("order_id").is_some() {
            "cancel_order"
        } else {
            "place_order"
        };
        assert_eq!(backend.execute(op, p).unwrap_err().code, "invalid_params");
    }
}

#[test]
fn mutation_is_never_retried_and_redirect_is_not_followed() {
    for status in [302, 429, 500] {
        let fixture = Fixture::new(vec![reply(
            status,
            "Location: http://127.0.0.1:1/leak\r\nRetry-After: 0\r\n",
            "{}",
        )]);
        let e = fixture
            .backend(true)
            .execute("cancel_all", json!({"market":"spot","symbol":"BTCUSDT"}))
            .unwrap_err();
        assert!(["http_error", "rate_limited"].contains(&e.code.as_str()));
        fixture.request();
        fixture.finish(); // Fixture services exactly one request; a retry would fail the outcome.
    }
}

#[test]
fn read_retry_obeys_retry_after_and_rate_spacing() {
    let fixture = Fixture::new(vec![
        reply(429, "Retry-After: 1\r\n", "{}"),
        reply(200, "", "{}"),
    ]);
    assert_eq!(
        fixture
            .backend(false)
            .execute("health", json!({"market":"spot"}))
            .unwrap()["ok"],
        true
    );
    let first = fixture.requests.recv().unwrap().0;
    let second = fixture.requests.recv().unwrap().0;
    assert!(second.duration_since(first) >= Duration::from_millis(990));
    fixture.finish();
}

#[test]
fn all_instances_share_the_public_private_rate_limit() {
    let fixture = Fixture::new(vec![
        reply(200, "", "{}"),
        reply(200, "", "{}"),
        reply(200, "", "{}"),
    ]);
    let first = fixture.backend(true);
    let second = fixture.backend(true);
    first.execute("health", json!({"market":"spot"})).unwrap();
    second
        .execute("cancel_all", json!({"market":"spot","symbol":"BTCUSDT"}))
        .unwrap();
    first.execute("health", json!({"market":"spot"})).unwrap();
    let times: Vec<_> = (0..3).map(|_| fixture.requests.recv().unwrap().0).collect();
    for pair in times.windows(2) {
        assert!(pair[1].duration_since(pair[0]) >= Duration::from_millis(200));
    }
    fixture.finish();
}

#[test]
fn response_size_and_invalid_json_are_bounded_and_redacted() {
    for response in [
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_BYTES + 1
        ),
        // No declared length: enforce the streaming bound rather than trusting headers.
        format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{}",
            " ".repeat(MAX_BYTES + 1)
        ),
        reply(200, "", "not JSON fixture-key fixture-secret"),
        reply(200, "", "null"),
    ] {
        let fixture = Fixture::new(vec![response]);
        let e = fixture
            .backend(true)
            .execute("balances", json!({"market":"spot"}))
            .unwrap_err();
        assert!(["response_too_large", "invalid_response"].contains(&e.code.as_str()));
        assert!(!format!("{e:?}").contains("fixture-"));
        fixture.request();
        fixture.finish();
    }
}

#[test]
fn endpoint_and_credential_configuration_is_safe() {
    for endpoint in [
        "http://example.com",
        "https://fixture-key:fixture-secret@example.com",
        "https://example.com/x",
        "https://example.com?key=fixture-secret",
        "https://example.com/#fragment",
        "file:///tmp/test",
    ] {
        let e = Ourbit::with_endpoints(endpoint, FUTURES_ENDPOINT, None, None).unwrap_err();
        assert_eq!(e.code, "configuration");
        assert!(!format!("{e:?}").contains("fixture-"));
    }
    assert!(Credentials::new("key\r\nInjected: evil", "secret").is_err());
    assert!(Credentials::new("key", "").is_err());
    assert!(
        !format!(
            "{:?}",
            Credentials::new("fixture-key", "fixture-secret").unwrap()
        )
        .contains("fixture-")
    );
}

#[test]
#[ignore = "Opt-in public historical GETs only; no credentials, requires network access"]
fn live_public_historical_candles() {
    use std::sync::Arc;
    use tradeslope_sdk::{BackendHost, Permissions};
    // Start from exchange-selected latest bars, never from the local clock.
    let backend = Ourbit::with_endpoints(SPOT_ENDPOINT, FUTURES_ENDPOINT, None, None).unwrap();
    let host = BackendHost::new(vec![Arc::new(backend)]).unwrap();
    let client = host.client(Permissions::default());
    for (market, interval, limit) in ["spot", "futures"].into_iter().flat_map(|market| {
        ["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1M"]
            .map(move |interval| (market, interval, if interval == "1h" { 200 } else { 2 }))
    }) {
        let mut params = json!({
            "market":market,"symbol":"BTC_USDT","interval":interval,"limit":limit
        });
        let mut previous_oldest = None;
        for page in 0..3 {
            let result = client
                .call("ourbit", "candles", params.clone())
                .unwrap_or_else(|error| panic!("{market} {interval} page {page}: {error}"));
            let rows = result.as_array().unwrap();
            assert!(
                !rows.is_empty() && rows.len() <= 200,
                "{market} page {page}"
            );
            let times: Vec<i64> = rows
                .iter()
                .map(|row| row["time"].as_i64().unwrap())
                .collect();
            assert!(times.windows(2).all(|pair| pair[0] < pair[1]));
            if let Some(oldest) = previous_oldest {
                assert!(
                    times.iter().all(|time| *time < oldest),
                    "{market} overlapping page {page}"
                );
                assert!(
                    times
                        .iter()
                        .all(|time| *time <= params["end"].as_i64().unwrap())
                );
            }
            let oldest = times[0];
            eprintln!(
                "{market} {interval} page {page}: {} bars, {}..{}",
                rows.len(),
                oldest,
                times.last().unwrap()
            );
            params["end"] = json!(oldest - 1);
            previous_oldest = Some(oldest);
        }
    }
}

#[test]
#[ignore = "Opt-in public GETs only; requires network access and may be blocked by regional/WAF rules"]
fn live_public() {
    use std::sync::Arc;
    use tradeslope_sdk::{BackendHost, Permissions};
    // Deliberately never read credentials from the environment in a live test.
    let backend = Ourbit::with_endpoints(SPOT_ENDPOINT, FUTURES_ENDPOINT, None, None).unwrap();
    let host = BackendHost::new(vec![Arc::new(backend)]).unwrap();
    let client = host.client(Permissions::default());
    for market in ["spot", "futures"] {
        for operation in [
            "health",
            "markets",
            "ticker",
            "candles",
            "order_book",
            "trades",
        ] {
            let mut params = json!({"market":market});
            if !["health", "markets"].contains(&operation) {
                params["symbol"] = "BTC_USDT".into();
            }
            if ["candles", "order_book", "trades"].contains(&operation) {
                params["limit"] = 2.into();
            }
            if operation == "candles" {
                params["interval"] = "1m".into();
            }
            let result = client.call("ourbit", operation, params);
            assert!(result.is_ok(), "{market} {operation}: {result:?}");
        }
        for interval in ["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1w", "1M"] {
            if market == "spot" && interval == "1w" {
                continue;
            }
            let result = client.call(
                "ourbit",
                "candles",
                json!({
                    "market":market,"symbol":"BTCUSDT","interval":interval,"limit":2
                }),
            );
            let candles = result.unwrap_or_else(|e| panic!("{market} {interval}: {e}"));
            assert!(
                candles
                    .as_array()
                    .is_some_and(|rows| !rows.is_empty() && rows.len() <= 2),
                "{market} {interval}: expected nonempty bounded candles"
            );
        }
    }
    assert!(
        client
            .call(
                "ourbit",
                "funding",
                json!({"market":"futures","symbol":"BTCUSDT"})
            )
            .is_ok()
    );
}
