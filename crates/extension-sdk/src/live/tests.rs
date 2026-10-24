use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

// Unit-test injection only: consumers have no snapshot publication capability.
impl BackendClient {
    fn deliver_for_test(
        &self,
        backend: &str,
        operation: &str,
        params: Value,
        value: Value,
    ) -> BackendResult<()> {
        let (_, op) = self.validate_query(backend, operation, &params)?;
        op.output
            .validate(&value)
            .map_err(|error| BackendError::new("invalid_output", error.message))?;
        let queries = self.live.queries.lock().unwrap();
        let query = queries
            .get(&QueryKey::new(backend, operation, &params))
            .and_then(Weak::upgrade)
            .unwrap();
        update(&mut query.state.lock().unwrap(), Ok(value));
        Ok(())
    }
}

type Gate = Arc<(Mutex<bool>, Condvar)>;
type Fixture = (BackendHost, Arc<AtomicUsize>, Gate);

struct TestBackend {
    calls: Arc<AtomicUsize>,
    gate: Gate,
}

impl Backend for TestBackend {
    fn manifest(&self) -> BackendManifest {
        let mut action = Operation::query("action", "Action", Schema::Any, Schema::Any);
        action.kind = OperationKind::Action;
        BackendManifest {
            id: "test".into(),
            title: "Test".into(),
            version: "1".into(),
            operations: vec![
                Operation::query("query", "Query", Schema::Any, Schema::Any),
                action,
            ],
        }
    }

    fn execute(&self, _: &str, params: Value) -> BackendResult<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (lock, wake) = &*self.gate;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = wake.wait(open).unwrap();
        }
        Ok(params)
    }
}

fn fixture(open: bool) -> Fixture {
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new((Mutex::new(open), Condvar::new()));
    let host = BackendHost::new(vec![Arc::new(TestBackend {
        calls: calls.clone(),
        gate: gate.clone(),
    })])
    .unwrap();
    (host, calls, gate)
}

fn bound(client: BackendClient, kind: OperationKind) -> BoundInputs {
    BoundInputs::new(
        client,
        vec![InputPort {
            id: "in".into(),
            title: "Input".into(),
            kind,
            parameters: Schema::Any,
            output: Schema::Any,
            required: true,
        }],
        BTreeMap::from([(
            "in".into(),
            PortBinding {
                backend: "test".into(),
                operation: if kind == OperationKind::Query {
                    "query"
                } else {
                    "action"
                }
                .into(),
                params: json!({"constant": 1}),
                fields: BTreeMap::new(),
                parameter_fields: BTreeMap::new(),
            },
        )]),
    )
    .unwrap()
}

fn until(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !check() {
        assert!(Instant::now() < deadline, "condition timed out");
        std::thread::yield_now();
    }
}

fn receive(sub: &mut InputSubscription) -> InputSnapshot {
    let mut result = None;
    until(|| {
        if let Ok(snapshot) = sub.try_recv()
            && snapshot.status != SubscriptionStatus::Loading
        {
            result = Some(snapshot);
        }
        result.is_some()
    });
    result.unwrap()
}

#[test]
fn mapped_query_shares_single_flight_and_drop_cancels_worker() {
    let (host, calls, gate) = fixture(false);
    let inputs = bound(host.client(Permissions::default()), OperationKind::Query);
    // Different frontend values map to the identical constant backend argument.
    let first = inputs.subscribe("in", json!({"constant": 7})).unwrap();
    let second = inputs.subscribe("in", json!({"constant": 9})).unwrap();
    let query = first.query.clone();
    assert!(Arc::ptr_eq(&first.query, &second.query));
    until(|| calls.load(Ordering::SeqCst) == 1);
    first.refresh();
    second.refresh();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(first);
    assert_eq!(query.state.lock().unwrap().subscribers, 1);
    drop(second);
    assert!(host.live.queries.lock().unwrap().is_empty());
    // Cancellation releases the subscription worker even with backend I/O stuck.
    until(|| Arc::strong_count(&query) == 1);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}

#[test]
fn delivery_is_bounded_change_only_and_validated_through_bound_request() {
    let (host, _, _) = fixture(true);
    let client = host.client(Permissions::default());
    let inputs = bound(client.clone(), OperationKind::Query);
    let mut sub = inputs.subscribe("in", json!({})).unwrap();
    assert_eq!(receive(&mut sub).value, Some(json!({"constant":1})));
    for n in 0..100 {
        client
            .deliver_for_test("test", "query", json!({"constant":1}), json!(n))
            .unwrap();
    }
    assert_eq!(sub.try_recv().unwrap().value, Some(json!(99)));
    assert_eq!(sub.try_recv(), Err(mpsc::TryRecvError::Empty));
    client
        .deliver_for_test("test", "query", json!({"constant":1}), json!(99))
        .unwrap();
    assert_eq!(sub.try_recv(), Err(mpsc::TryRecvError::Empty));
    // Exercise the same projection validator used by BoundRequest.
    sub.output = Schema::Number;
    client
        .deliver_for_test("test", "query", json!({"constant":1}), json!("invalid"))
        .unwrap();
    assert_eq!(
        sub.try_recv().unwrap().error.unwrap().code,
        "invalid_output"
    );
}

#[test]
fn invalidation_refreshes_and_actions_never_execute() {
    let (host, calls, _) = fixture(true);
    let client = host.client(Permissions::default());
    let actions = bound(client.clone(), OperationKind::Action);
    assert_eq!(
        actions.subscribe("in", json!({})).err().unwrap().code,
        "not_query"
    );
    assert_eq!(
        client
            .deliver_for_test("test", "action", json!({}), json!(1))
            .unwrap_err()
            .code,
        "not_query"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let inputs = bound(client, OperationKind::Query);
    let mut sub = inputs.subscribe("in", json!({})).unwrap();
    receive(&mut sub);
    host.invalidate("test");
    until(|| calls.load(Ordering::SeqCst) == 2);
    // Wait for the pending request to be consumed without a duplicate emission.
    let query = sub.query.clone();
    until(|| !query.state.lock().unwrap().dirty);
    assert_eq!(sub.try_recv(), Err(mpsc::TryRecvError::Empty));
    assert!(host.set_poll_interval(Duration::ZERO).is_err());
}

#[test]
fn reset_clears_existing_and_new_subscribers_and_fences_old_response() {
    struct Controlled {
        started: mpsc::Sender<()>,
        replies: Mutex<mpsc::Receiver<Value>>,
    }
    impl Backend for Controlled {
        fn manifest(&self) -> BackendManifest {
            BackendManifest {
                id: "test".into(),
                title: "Test".into(),
                version: "1".into(),
                operations: vec![Operation::query("query", "Query", Schema::Any, Schema::Any)],
            }
        }
        fn execute(&self, _: &str, _: Value) -> BackendResult<Value> {
            self.started.send(()).unwrap();
            Ok(self.replies.lock().unwrap().recv().unwrap())
        }
    }
    let (started, starts) = mpsc::channel();
    let (reply, replies) = mpsc::channel();
    let host = BackendHost::new(vec![Arc::new(Controlled {
        started,
        replies: Mutex::new(replies),
    })])
    .unwrap();
    let client = host.client(Permissions::default());
    let inputs = bound(client.clone(), OperationKind::Query);
    let mut sub = inputs.subscribe("in", json!({})).unwrap();
    starts.recv_timeout(Duration::from_secs(3)).unwrap();
    reply.send(json!("account one")).unwrap();
    assert_eq!(receive(&mut sub).value, Some(json!("account one")));
    sub.refresh();
    starts.recv_timeout(Duration::from_secs(3)).unwrap();
    client.resetter()("test");
    let reset = sub.try_recv().unwrap();
    assert_eq!(reset.status, SubscriptionStatus::Loading);
    assert_eq!(reset.value, None);
    assert_eq!(reset.error, None);
    let mut new = inputs.subscribe("in", json!({})).unwrap();
    assert_eq!(new.try_recv().unwrap(), reset);
    reply.send(json!("old account in-flight")).unwrap();
    // The third execution only starts after the stale response was consumed.
    starts.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(sub.query.state.lock().unwrap().value, None);
    assert_eq!(sub.try_recv(), Err(mpsc::TryRecvError::Empty));
    reply.send(json!("new account")).unwrap();
    assert_eq!(receive(&mut sub).value, Some(json!("new account")));
    assert_eq!(receive(&mut new).value, Some(json!("new account")));
}

#[test]
fn shared_raw_query_projects_and_deduplicates_each_input_independently() {
    let (host, calls, _) = fixture(true);
    let client = host.client(Permissions::default());
    let args = json!({"constant":1});
    let mut subscriptions = ["first", "second"].map(|name| {
        client
            .subscribe_input(
                "test",
                "query",
                args.clone(),
                Schema::object(BTreeMap::from([(
                    name.into(),
                    Field {
                        schema: Schema::Number,
                        required: true,
                        description: String::new(),
                        default: None,
                    },
                )])),
                BTreeMap::from([(name.into(), "constant".into())]),
            )
            .unwrap()
    });
    assert_eq!(
        receive(&mut subscriptions[0]).value,
        Some(json!({"first":1}))
    );
    assert_eq!(
        receive(&mut subscriptions[1]).value,
        Some(json!({"second":1}))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    client
        .deliver_for_test(
            "test",
            "query",
            args.clone(),
            json!({"constant":1,"ignored":2}),
        )
        .unwrap();
    for sub in &mut subscriptions {
        assert_eq!(sub.try_recv(), Err(mpsc::TryRecvError::Empty));
    }
    // Transport errors retain the last good value and recovery is observable
    // even when the actual quote has not changed.
    let query = subscriptions[0].query.clone();
    update(
        &mut query.state.lock().unwrap(),
        Err(BackendError::new("offline", "No connection")),
    );
    let failed = subscriptions[0].try_recv().unwrap();
    assert_eq!(failed.value, Some(json!({"first":1})));
    assert_eq!(failed.status, SubscriptionStatus::Error);
    client
        .deliver_for_test("test", "query", args, json!({"constant":1}))
        .unwrap();
    assert_eq!(
        subscriptions[0].try_recv().unwrap().status,
        SubscriptionStatus::Live
    );
}

#[test]
fn weak_runtime_invalidator_does_not_keep_host_alive() {
    let (host, _, _) = fixture(true);
    let weak = Arc::downgrade(&host.live);
    let notify = host.client(Permissions::default()).invalidator();
    let reset = host.client(Permissions::default()).resetter();
    drop(host);
    assert!(weak.upgrade().is_none());
    notify("test"); // Safe after host shutdown.
    reset("test");
}
