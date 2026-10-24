use super::*;
use serde_json::json;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Mock {
    manifest: BackendManifest,
    calls: Arc<AtomicUsize>,
}

fn mock(id: &str) -> Mock {
    Mock {
        manifest: BackendManifest {
            id: id.into(),
            title: id.into(),
            version: "1.0.0".into(),
            operations: [
                ("echo", Access::Public),
                ("read", Access::PrivateRead),
                ("trade", Access::Trade),
                ("error", Access::Public),
                ("panic", Access::Public),
            ]
            .into_iter()
            .map(|(id, access)| Operation {
                id: id.into(),
                title: id.into(),
                access,
                kind: OperationKind::Query,
                parameters: Schema::Any,
                output: Schema::Any,
            })
            .collect(),
        },
        calls: Arc::new(AtomicUsize::new(0)),
    }
}

impl Backend for Mock {
    fn manifest(&self) -> BackendManifest {
        self.manifest.clone()
    }

    fn execute(&self, operation: &str, params: Value) -> BackendResult<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match operation {
            "panic" => panic!("sensitive backend detail"),
            "error" => Err(BackendError::new("mock_error", "Expected backend error")),
            _ => Ok(json!({
                "backend": self.manifest.id,
                "operation": operation,
                "params": params,
                "thread": format!("{:?}", std::thread::current().id()),
            })),
        }
    }
}

fn code<T>(result: BackendResult<T>) -> String {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => error.code,
    }
}

#[test]
fn routing_manifests_and_worker_thread() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<BackendClient>();
    let host = BackendHost::new(vec![Arc::new(mock("one")), Arc::new(mock("two"))]).unwrap();
    let client = host.client(Permissions::default());
    assert_eq!(host.manifests(), client.manifests());
    let mut detached = client.manifests();
    detached[0].id = "changed".into();
    assert_eq!(host.manifests()[0].id, "one");
    drop(host);
    let result = client.clone().call("two", "echo", json!({"x": 7})).unwrap();
    assert_eq!(result["backend"], "two");
    assert_eq!(result["operation"], "echo");
    assert_eq!(result["params"], json!({"x": 7}));
    assert_ne!(
        result["thread"],
        format!("{:?}", std::thread::current().id())
    );
}

#[test]
fn permissions_and_unknown_routes_never_execute() {
    let backend = mock("mock");
    let calls = backend.calls.clone();
    let host = BackendHost::new(vec![Arc::new(backend)]).unwrap();
    let public = host.client(Permissions::default());
    for operation in ["read", "trade"] {
        assert_eq!(
            code(public.submit("mock", operation, Value::Null)),
            "permission_denied"
        );
    }
    assert_eq!(
        code(public.submit("missing", "echo", Value::Null)),
        "unknown_backend"
    );
    assert_eq!(
        code(public.submit("mock", "missing", Value::Null)),
        "unknown_operation"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let read = host.client(Permissions {
        private_read: true,
        trade: false,
    });
    read.call("mock", "read", Value::Null).unwrap();
    assert_eq!(
        code(read.submit("mock", "trade", Value::Null)),
        "permission_denied"
    );
    let trade = host.client(Permissions {
        private_read: false,
        trade: true,
    });
    trade.call("mock", "trade", Value::Null).unwrap();
    assert_eq!(
        code(trade.submit("mock", "read", Value::Null)),
        "permission_denied"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn backend_errors_and_panics_leave_worker_usable() {
    let host = BackendHost::new(vec![Arc::new(mock("mock"))]).unwrap();
    let client = host.client(Permissions::default());
    assert_eq!(
        client.call("mock", "error", Value::Null),
        Err(BackendError::new("mock_error", "Expected backend error"))
    );
    let error = client.call("mock", "panic", Value::Null).unwrap_err();
    assert_eq!(error, internal_error());
    assert!(!serde_json::to_string(&error).unwrap().contains("sensitive"));
    client.call("mock", "echo", Value::Null).unwrap();
}

#[test]
fn backend_registry_validation() {
    assert_eq!(
        code(BackendHost::new(vec![
            Arc::new(mock("a")),
            Arc::new(mock("a"))
        ])),
        "duplicate_backend"
    );
    let mut backend = mock("a");
    backend
        .manifest
        .operations
        .push(backend.manifest.operations[0].clone());
    assert_eq!(
        code(BackendHost::new(vec![Arc::new(backend)])),
        "duplicate_operation"
    );
    for field in ["id", "version", "operation"] {
        let mut backend = mock("a");
        match field {
            "id" => backend.manifest.id.clear(),
            "version" => backend.manifest.version = "  ".into(),
            _ => backend.manifest.operations[0].id.clear(),
        }
        assert_eq!(
            code(BackendHost::new(vec![Arc::new(backend)])),
            "invalid_manifest"
        );
    }
}

struct Gated {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

#[test]
fn manifest_panics_are_redacted_and_empty_registries_are_valid() {
    struct PanickingManifest;
    impl Backend for PanickingManifest {
        fn manifest(&self) -> BackendManifest {
            panic!("secret manifest detail");
        }

        fn execute(&self, _: &str, _: Value) -> BackendResult<Value> {
            unreachable!()
        }
    }
    let error = match BackendHost::new(vec![Arc::new(PanickingManifest)]) {
        Ok(_) => panic!("expected manifest rejection"),
        Err(error) => error,
    };
    assert_eq!(error, internal_error());
    let empty = BackendHost::new(vec![]).unwrap();
    assert!(empty.manifests().is_empty());
    assert_eq!(
        code(
            empty
                .client(Permissions::default())
                .submit("a", "b", Value::Null)
        ),
        "unknown_backend"
    );
    assert!(
        ExtensionRegistry::new(vec![])
            .unwrap()
            .manifests()
            .is_empty()
    );
}

impl Backend for Gated {
    fn manifest(&self) -> BackendManifest {
        mock("gate").manifest
    }

    fn execute(&self, _: &str, params: Value) -> BackendResult<Value> {
        self.entered.send(()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        Ok(params)
    }
}

#[test]
fn bounded_queue_pending_poll_and_dropped_replies() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let host = BackendHost::new(vec![Arc::new(Gated {
        entered: entered_tx,
        release: Mutex::new(release_rx),
    })])
    .unwrap();
    let client = host.client(Permissions::default());
    let first = client.submit("gate", "echo", Value::Null).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(first.try_recv(), Err(mpsc::TryRecvError::Empty));
    drop(first); // A lost reply must not kill the worker.
    for _ in 0..QUEUE_CAPACITY {
        drop(client.submit("gate", "echo", Value::Null).unwrap());
    }
    assert_eq!(
        code(client.submit("gate", "echo", Value::Null)),
        "queue_full"
    );
    // Drain deterministically, leaving the final request available for polling.
    for _ in 0..QUEUE_CAPACITY {
        release_tx.send(()).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    let final_request = client.submit("gate", "echo", json!(42)).unwrap();
    release_tx.send(()).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    release_tx.send(()).unwrap();
    assert_eq!(
        final_request.wait(Duration::from_secs(5)).unwrap(),
        json!(42)
    );
}

#[test]
fn disconnect_and_timeout_are_errors() {
    let host = BackendHost::new(vec![Arc::new(mock("mock"))]).unwrap();
    let mut client = host.client(Permissions::default());
    let (sender, receiver) = mpsc::sync_channel(1);
    drop(receiver);
    client.sender = sender;
    assert_eq!(
        code(client.submit("mock", "echo", Value::Null)),
        "disconnected"
    );
    let (sender, receiver) = mpsc::channel();
    let pending = PendingRequest { receiver };
    drop(sender);
    assert_eq!(pending.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    assert_eq!(code(pending.wait(Duration::ZERO)), "disconnected");
    let (_sender, receiver) = mpsc::channel();
    assert_eq!(
        code(PendingRequest { receiver }.wait(Duration::ZERO)),
        "timeout"
    );
    let (sender, receiver) = mpsc::channel();
    sender.send(Ok(json!(1))).unwrap();
    assert_eq!(
        PendingRequest { receiver }.try_recv().unwrap().unwrap(),
        json!(1)
    );
    assert_eq!(CALL_TIMEOUT, Duration::from_secs(30));
}

#[test]
fn parameter_limit_counts_encoded_bytes_inclusively() {
    let backend = mock("mock");
    let calls = backend.calls.clone();
    let host = BackendHost::new(vec![Arc::new(backend)]).unwrap();
    let client = host.client(Permissions::default());
    // Two bytes for JSON string quotes.
    client
        .call(
            "mock",
            "echo",
            Value::String("a".repeat(MAX_PARAMS_BYTES - 2)),
        )
        .unwrap();
    assert_eq!(
        code(client.submit(
            "mock",
            "echo",
            Value::String("a".repeat(MAX_PARAMS_BYTES - 1))
        )),
        "params_too_large"
    );
    // Escaped newlines require two bytes each in serialized JSON.
    assert_eq!(
        code(client.submit(
            "mock",
            "echo",
            Value::String("\n".repeat(MAX_PARAMS_BYTES / 2))
        )),
        "params_too_large"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn bridge_protocol_and_normalized_serialization() {
    let host = BackendHost::new(vec![Arc::new(mock("mock"))]).unwrap();
    let client = host.client(Permissions::default());
    let mut request: BackendRequest = serde_json::from_value(json!({
        "version": 1, "id": 99, "backend": "mock", "operation": "echo", "params": null
    }))
    .unwrap();
    let result = client
        .submit_request(request.clone())
        .unwrap()
        .wait(Duration::from_secs(5))
        .unwrap();
    let response = BackendResponse {
        version: 1,
        id: request.id,
        result: Some(result),
        error: None,
    };
    let response: BackendResponse =
        serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();
    assert_eq!(response.id, 99);
    assert!(response.result.is_some());
    assert!(response.error.is_none());
    request.version = 2;
    assert_eq!(code(client.submit_request(request)), "unsupported_version");
    for (access, name) in [
        (Access::Public, "public"),
        (Access::PrivateRead, "private_read"),
        (Access::Trade, "trade"),
    ] {
        assert_eq!(serde_json::to_value(access).unwrap(), json!(name));
    }
    let candle = Candle {
        time: 1_700_000_000,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 12.0,
    };
    let value = serde_json::to_value(&candle).unwrap();
    assert_eq!(value["time"], json!(1_700_000_000));
    assert_eq!(serde_json::from_value::<Candle>(value).unwrap(), candle);
    let market = Market {
        symbol: "BTC/USD".into(),
        base: "BTC".into(),
        quote: "USD".into(),
        market: "spot".into(),
    };
    assert_eq!(
        serde_json::from_value::<Market>(serde_json::to_value(&market).unwrap()).unwrap(),
        market
    );
    let ticker: Ticker = serde_json::from_value(json!({"symbol":"BTC/USD","last":10.0})).unwrap();
    assert_eq!(ticker.bid, None);
    assert_eq!(serde_json::to_value(ticker).unwrap()["ask"], Value::Null);
    let error = BackendError::new("code", "message");
    assert_eq!(error.to_string(), "code: message");
    fn is_error(_: &dyn std::error::Error) {}
    is_error(&error);
}

fn extension(id: &str, card: &str) -> ExtensionManifest {
    ExtensionManifest {
        id: id.into(),
        title: id.into(),
        version: "1.0.0".into(),
        cards: vec![CardManifest {
            id: card.into(),
            title: card.into(),
            renderer: Renderer::Native,
            inputs: vec![],
        }],
    }
}

#[test]
fn extension_registration_validation_and_serialization() {
    let first = extension("one", "card-one");
    let registry =
        ExtensionRegistry::new(vec![first.clone(), extension("two", "card-two")]).unwrap();
    assert_eq!(registry.manifests()[0], first);
    assert_eq!(
        code(ExtensionRegistry::new(vec![first.clone(), first.clone()])),
        "duplicate_extension"
    );
    assert_eq!(
        code(ExtensionRegistry::new(vec![
            first.clone(),
            extension("two", "card-one")
        ])),
        "duplicate_card"
    );
    let mut duplicate = first.clone();
    duplicate.cards.push(duplicate.cards[0].clone());
    assert_eq!(
        code(ExtensionRegistry::new(vec![duplicate])),
        "duplicate_card"
    );
    for field in ["id", "version", "card"] {
        let mut invalid = first.clone();
        match field {
            "id" => invalid.id.clear(),
            "version" => invalid.version = " ".into(),
            _ => invalid.cards[0].id.clear(),
        }
        assert_eq!(
            code(ExtensionRegistry::new(vec![invalid])),
            "invalid_manifest"
        );
    }
    let value = serde_json::to_value(&first).unwrap();
    assert_eq!(value["cards"][0]["renderer"], "native");
    assert_eq!(
        serde_json::from_value::<ExtensionManifest>(value).unwrap(),
        first
    );
    assert_eq!(
        serde_json::to_value(Renderer::JavaScript).unwrap(),
        "java_script"
    );
}
