use super::*;
use crate::{Field, Operation, Permissions};
use serde_json::json;
use std::sync::atomic::AtomicUsize;

#[test]
fn resilient_startup_keeps_failed_provider_available_for_explicit_retry() {
    let failed = Provider::new("failed");
    failed.fail_start.store(true, Ordering::SeqCst);
    let healthy = Provider::new("healthy");
    let (mut runtime, errors) = BackendRuntime::new_resilient(
        Database::in_memory().unwrap(),
        vec![failed.clone(), healthy.clone()],
    )
    .unwrap();
    assert_eq!(errors.len(), 1);
    let states = runtime.states();
    assert!(
        states
            .iter()
            .any(|state| state.id == "failed" && state.enabled && !state.running)
    );
    assert!(
        states
            .iter()
            .any(|state| state.id == "healthy" && state.running)
    );
    runtime.set_enabled("failed", false).unwrap();
    failed.fail_start.store(false, Ordering::SeqCst);
    runtime.set_enabled("failed", true).unwrap();
    assert!(runtime.states().iter().all(|state| state.running));
}

#[test]
fn revoked_installation_consent_overrides_stale_enabled_id() {
    let database = Database::in_memory().unwrap();
    {
        let mut runtime =
            BackendRuntime::new(database.clone(), vec![Provider::new("replacement")]).unwrap();
        runtime.set_enabled("replacement", true).unwrap();
    }
    database.revoke_backend_consent("replacement").unwrap();
    let replacement = Provider::new("replacement");
    let mut runtime = BackendRuntime::new(database, vec![replacement.clone()]).unwrap();
    assert_eq!(replacement.starts.load(Ordering::SeqCst), 0);
    assert!(!runtime.states()[0].enabled);
    runtime.set_enabled("replacement", true).unwrap();
    assert_eq!(replacement.starts.load(Ordering::SeqCst), 1);
}

#[test]
fn enabled_reads_wait_for_configuration_instead_of_reporting_disabled() {
    let runtime = Arc::new(
        BackendRuntime::new(Database::in_memory().unwrap(), vec![Provider::new("a")]).unwrap(),
    );
    let host = crate::BackendHost::new(runtime.backends()).unwrap();
    let client = host.client(Permissions::default());
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    let configuring = runtime.clone();
    let task = std::thread::spawn(move || {
        configuring
            .entry("a")
            .unwrap()
            .configure(|| {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(2)).unwrap();
                Ok(())
            })
            .unwrap();
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let request = client.submit("a", "read", json!({})).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    assert!(matches!(
        request.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    release.send(()).unwrap();
    task.join().unwrap();
    assert_eq!(request.wait(Duration::from_secs(2)).unwrap(), json!(42));
}

#[test]
fn selected_account_is_live_persistent_and_cleared_on_deletion() {
    let temporary = TestDatabase::new();
    let profile = AccountProfile {
        id: "main".into(),
        label: "Main".into(),
        env_refs: BTreeMap::from([("api_key".into(), "OURBIT_API_KEY".into())]),
    };
    {
        let provider = Provider::new("a");
        let mut runtime = BackendRuntime::new(temporary.open(), vec![provider.clone()]).unwrap();
        let services = provider.services.lock().unwrap()[0].clone();
        assert_eq!(runtime.active_account("a").unwrap(), None);
        assert_eq!(services.active_account().unwrap(), None);
        runtime.save_accounts("a", vec![profile.clone()]).unwrap();
        assert_eq!(runtime.active_account("a").unwrap(), None);
        assert_eq!(
            runtime
                .select_account("a", Some("missing".into()))
                .unwrap_err()
                .code,
            "unknown_account"
        );
        runtime.select_account("a", Some("main".into())).unwrap();
        assert_eq!(services.active_account().unwrap(), Some(profile.clone()));
        runtime.save_accounts("a", vec![profile.clone()]).unwrap();
        assert_eq!(runtime.active_account("a").unwrap(), Some("main".into()));
    }
    {
        let provider = Provider::new("a");
        let mut runtime = BackendRuntime::new(temporary.open(), vec![provider.clone()]).unwrap();
        let services = provider.services.lock().unwrap()[0].clone();
        assert_eq!(runtime.active_account("a").unwrap(), Some("main".into()));
        assert_eq!(services.active_account().unwrap(), Some(profile.clone()));
        assert!(runtime.select_account("a", Some("missing".into())).is_err());
        assert_eq!(runtime.active_account("a").unwrap(), Some("main".into()));
        runtime.select_account("a", None).unwrap();
        assert_eq!(services.active_account().unwrap(), None);
        runtime.select_account("a", Some("main".into())).unwrap();
        runtime.save_accounts("a", vec![]).unwrap();
        assert_eq!(runtime.active_account("a").unwrap(), None);
        assert_eq!(services.active_account().unwrap(), None);
    }
    let runtime = BackendRuntime::new(temporary.open(), vec![Provider::new("a")]).unwrap();
    assert_eq!(runtime.active_account("a").unwrap(), None);
    assert!(runtime.accounts("a").unwrap().is_empty());
}

struct Provider {
    id: &'static str,
    starts: AtomicUsize,
    stops: AtomicUsize,
    fail_start: AtomicBool,
    services: Mutex<Vec<BackendServices>>,
}

impl Provider {
    fn new(id: &'static str) -> Arc<Self> {
        Arc::new(Self {
            id,
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            fail_start: AtomicBool::new(false),
            services: Mutex::new(Vec::new()),
        })
    }
}

impl Backend for Provider {
    fn manifest(&self) -> BackendManifest {
        BackendManifest {
            id: self.id.into(),
            title: self.id.into(),
            version: "1".into(),
            operations: vec![Operation::query("read", "Read", Schema::Any, Schema::Any)],
        }
    }

    fn execute(&self, _: &str, _: Value) -> BackendResult<Value> {
        Ok(json!(42))
    }

    fn settings(&self) -> ExtensionSettings {
        ExtensionSettings {
            schema: Schema::object(BTreeMap::from([(
                "poll".into(),
                Field {
                    schema: Schema::Integer,
                    required: true,
                    description: "Poll interval".into(),
                    default: Some(json!(5)),
                },
            )])),
            defaults: json!({"poll": 5}),
        }
    }

    fn validate_settings(&self, settings: &Value) -> BackendResult<()> {
        if settings["poll"].as_u64().is_some_and(|poll| poll > 0) {
            Ok(())
        } else {
            Err(error("invalid_settings", "Poll interval must be positive"))
        }
    }

    fn start(&self, services: BackendServices) -> BackendResult<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.services.lock().unwrap().push(services);
        if self.fail_start.load(Ordering::SeqCst) {
            Err(error("start_failed", "Test failure"))
        } else {
            Ok(())
        }
    }

    fn stop(&self) -> BackendResult<()> {
        assert!(
            self.services
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .cancellation
                .is_cancelled()
        );
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Files stay beneath this SDK crate, never the system temp directory.
struct TestDatabase(std::path::PathBuf);
impl TestDatabase {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "runtime-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn open(&self) -> Database {
        Database::open(self.0.join("runtime.sqlite")).unwrap()
    }
}
impl Drop for TestDatabase {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn persistence_namespaces_and_metadata_isolation() {
    let temporary = TestDatabase::new();
    {
        let database = temporary.open();
        database
            .namespace("a")
            .unwrap()
            .set("settings", &json!({"any": [1,2]}))
            .unwrap();
        database
            .namespace("a")
            .unwrap()
            .set("enabled", &json!(false))
            .unwrap();
        database
            .namespace("b")
            .unwrap()
            .set("settings", &json!("other"))
            .unwrap();
    }
    let database = temporary.open();
    assert_eq!(
        database.namespace("a").unwrap().get("settings").unwrap(),
        Some(json!({"any":[1,2]}))
    );
    assert_eq!(
        database.namespace("b").unwrap().get("settings").unwrap(),
        Some(json!("other"))
    );
    let a = Provider::new("a");
    let runtime = BackendRuntime::new(database.clone(), vec![a.clone()]).unwrap();
    assert_eq!(runtime.settings("a").unwrap(), json!({"poll":5}));
    assert!(runtime.states()[0].enabled);
    database.namespace("a").unwrap().remove("settings").unwrap();
    assert_eq!(
        database.namespace("a").unwrap().get("settings").unwrap(),
        None
    );
    assert!(database.namespace("").is_err());
}

#[test]
fn valid_settings_and_account_references_survive_reopen() {
    let temporary = TestDatabase::new();
    let profile = AccountProfile {
        id: "main".into(),
        label: "Trading account".into(),
        env_refs: BTreeMap::from([("api_key".into(), "OURBIT_API_KEY".into())]),
    };
    {
        let mut runtime = BackendRuntime::new(temporary.open(), vec![Provider::new("a")]).unwrap();
        runtime.save_settings("a", json!({"poll":10})).unwrap();
        assert!(runtime.save_settings("a", json!({"poll":"wrong"})).is_err());
        assert!(runtime.save_settings("a", json!({"poll":0})).is_err());
        assert!(
            runtime
                .save_settings("a", json!({"poll":10,"api_key":"secret"}))
                .is_err()
        );
        assert_eq!(runtime.settings("a").unwrap(), json!({"poll":10}));
        runtime.save_accounts("a", vec![profile.clone()]).unwrap();
        let mut invalid = profile.clone();
        invalid
            .env_refs
            .insert("api_key".into(), "actual-key-value!".into());
        assert!(runtime.save_accounts("a", vec![invalid]).is_err());
        assert!(
            runtime
                .save_accounts("a", vec![profile.clone(), profile.clone()])
                .is_err()
        );
        runtime.set_enabled("a", false).unwrap();
    }
    let provider = Provider::new("a");
    let runtime = BackendRuntime::new(temporary.open(), vec![provider.clone()]).unwrap();
    assert_eq!(runtime.settings("a").unwrap(), json!({"poll":10}));
    assert_eq!(runtime.accounts("a").unwrap(), vec![profile]);
    assert!(!runtime.states()[0].enabled);
    assert_eq!(provider.starts.load(Ordering::SeqCst), 0);
    assert!(
        serde_json::from_value::<AccountProfile>(json!({
            "id":"main","label":"x","env_refs":{},"api_key":"secret"
        }))
        .is_err()
    );
    let bytes = std::fs::read(temporary.0.join("runtime.sqlite")).unwrap();
    assert!(
        !bytes
            .windows(b"actual-key-value!".len())
            .any(|w| w == b"actual-key-value!")
    );
    assert!(!bytes.windows(b"secret".len()).any(|w| w == b"secret"));
}

#[test]
fn lifecycle_is_idempotent_and_host_wrappers_reject_disabled_calls() {
    let provider = Provider::new("a");
    let mut runtime =
        BackendRuntime::new(Database::in_memory().unwrap(), vec![provider.clone()]).unwrap();
    let host = crate::BackendHost::new(runtime.backends()).unwrap();
    let client = host.client(Permissions::default());
    assert_eq!(client.call("a", "read", json!({})).unwrap(), json!(42));
    runtime.set_enabled("a", true).unwrap();
    assert_eq!(provider.starts.load(Ordering::SeqCst), 1);
    runtime.set_enabled("a", false).unwrap();
    runtime.set_enabled("a", false).unwrap();
    assert_eq!(provider.stops.load(Ordering::SeqCst), 1);
    assert_eq!(
        client.call("a", "read", json!({})).unwrap_err().code,
        "backend_disabled"
    );
    runtime.set_enabled("a", true).unwrap();
    assert_eq!(provider.starts.load(Ordering::SeqCst), 2);
    let services = provider.services.lock().unwrap();
    assert!(services[0].cancellation.is_cancelled());
    assert!(!services[1].cancellation.is_cancelled());
    drop(services);
    drop(runtime);
    assert_eq!(provider.stops.load(Ordering::SeqCst), 2);
    assert_eq!(
        client.call("a", "read", json!({})).unwrap_err().code,
        "backend_disabled"
    );
}

#[test]
fn failed_start_cancels_and_cleans_up_without_enabling() {
    let provider = Provider::new("a");
    let mut runtime =
        BackendRuntime::new(Database::in_memory().unwrap(), vec![provider.clone()]).unwrap();
    runtime.set_enabled("a", false).unwrap();
    provider.fail_start.store(true, Ordering::SeqCst);
    assert!(runtime.set_enabled("a", true).is_err());
    assert!(!runtime.states()[0].enabled);
    assert!(!runtime.states()[0].running);
    assert_eq!(provider.stops.load(Ordering::SeqCst), 2);
    provider.fail_start.store(false, Ordering::SeqCst);
    runtime.set_enabled("a", true).unwrap();
    assert!(runtime.states()[0].running);
}

#[test]
fn live_services_and_change_notifications() {
    let provider = Provider::new("a");
    let mut runtime =
        BackendRuntime::new(Database::in_memory().unwrap(), vec![provider.clone()]).unwrap();
    let notifications = Arc::new(Mutex::new(Vec::new()));
    let output = notifications.clone();
    runtime
        .set_invalidator(Some(Arc::new(move |id| {
            output.lock().unwrap().push(id.to_owned())
        })))
        .unwrap();
    let services = provider.services.lock().unwrap()[0].clone();
    let resets = Arc::new(Mutex::new(Vec::new()));
    let reset_output = resets.clone();
    runtime
        .set_resetter(Some(Arc::new(move |id| {
            reset_output.lock().unwrap().push(id.to_owned());
        })))
        .unwrap();
    runtime.save_settings("a", json!({"poll": 20})).unwrap();
    assert_eq!(services.settings().unwrap(), json!({"poll":20}));
    runtime.save_accounts("a", Vec::new()).unwrap();
    services.notify_changed().unwrap();
    runtime.set_enabled("a", false).unwrap();
    assert_eq!(*notifications.lock().unwrap(), vec!["a"]);
    assert_eq!(*resets.lock().unwrap(), vec!["a", "a", "a", "a"]);
    runtime.set_invalidator(None).unwrap();
    services.notify_changed().unwrap();
    assert_eq!(notifications.lock().unwrap().len(), 1);
    runtime.set_enabled("a", true).unwrap();
    let profile = AccountProfile {
        id: "main".into(),
        label: "Main".into(),
        env_refs: BTreeMap::new(),
    };
    runtime.save_accounts("a", vec![profile]).unwrap();
    runtime.select_account("a", Some("main".into())).unwrap();
    runtime.save_accounts("a", vec![]).unwrap();
    assert_eq!(resets.lock().unwrap().len(), 8);
}

#[test]
fn disable_signals_cancellation_before_waiting_for_execution() {
    struct Blocking {
        services: Mutex<Option<BackendServices>>,
        entered: std::sync::mpsc::Sender<()>,
    }
    impl Backend for Blocking {
        fn manifest(&self) -> BackendManifest {
            Provider::new("blocking").manifest()
        }
        fn start(&self, services: BackendServices) -> BackendResult<()> {
            *self.services.lock().unwrap() = Some(services);
            Ok(())
        }
        fn execute(&self, _: &str, _: Value) -> BackendResult<Value> {
            let cancellation = self
                .services
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .cancellation
                .clone();
            self.entered.send(()).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while !cancellation.is_cancelled() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(json!(cancellation.is_cancelled()))
        }
    }
    let (entered, waiting) = std::sync::mpsc::channel();
    let mut runtime = BackendRuntime::new(
        Database::in_memory().unwrap(),
        vec![Arc::new(Blocking {
            services: Mutex::new(None),
            entered,
        })],
    )
    .unwrap();
    let backend = runtime.backends().remove(0);
    let execution = std::thread::spawn(move || backend.execute("read", json!({})));
    waiting.recv_timeout(Duration::from_secs(1)).unwrap();
    let (done, completed) = std::sync::mpsc::channel();
    let disable = std::thread::spawn(move || {
        runtime.set_enabled("blocking", false).unwrap();
        done.send(()).unwrap();
    });
    completed
        .recv_timeout(Duration::from_secs(1))
        .expect("disable must cancel before draining execute");
    assert_eq!(execution.join().unwrap().unwrap(), json!(true));
    disable.join().unwrap();
}

#[test]
fn constructor_failure_cleans_up_previously_started_providers() {
    let a = Provider::new("a");
    let b = Provider::new("b");
    b.fail_start.store(true, Ordering::SeqCst);
    assert!(
        BackendRuntime::new(Database::in_memory().unwrap(), vec![a.clone(), b.clone()]).is_err()
    );
    assert_eq!(a.starts.load(Ordering::SeqCst), 1);
    assert_eq!(a.stops.load(Ordering::SeqCst), 1);
    assert_eq!(b.stops.load(Ordering::SeqCst), 1);
}
