#![cfg(all(feature = "test-fixture", unix))]
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tradeslope_provider_process::{ProcessProvider, ProcessProviderConfig};
use tradeslope_sdk::runtime::{
    AccountProfile, BackendRuntime, Database, ExtensionSettings, ProviderSession,
};
use tradeslope_sdk::{Backend, BackendManifest, Operation, Schema};

static NEXT: AtomicUsize = AtomicUsize::new(0);
// macOS may serialize validation of a freshly rebuilt executable. Do not make
// unrelated fixture launches compete with tests measuring shutdown deadlines.
static PROCESS_TEST_GATE: Mutex<()> = Mutex::new(());
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

fn process_test() -> MutexGuard<'static, ()> {
    PROCESS_TEST_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn only_explicit_enablement_launches_and_persists() {
    let _gate = process_test();
    let config = config("serve");
    let path = config.db_path.clone();
    let database = Database::open(&path).unwrap();
    let mut runtime =
        BackendRuntime::new(database.clone(), vec![provider(config.clone())]).unwrap();
    assert!(!runtime.states()[0].enabled);
    assert!(!runtime.states()[0].running);
    runtime.set_enabled("fixture", true).unwrap();
    assert!(runtime.states()[0].running);
    drop(runtime);
    let mut runtime =
        BackendRuntime::new(database.clone(), vec![provider(config.clone())]).unwrap();
    assert!(runtime.states()[0].enabled);
    assert!(runtime.states()[0].running);
    runtime.set_enabled("fixture", false).unwrap();
    drop(runtime);
    let session = ProviderSession::new(
        provider(config.clone()),
        database.clone(),
        json!({}),
        vec![],
        None,
        None,
    )
    .unwrap();
    session
        .update(
            json!({}),
            vec![AccountProfile {
                id: "local".into(),
                label: "Local".into(),
                env_refs: BTreeMap::new(),
            }],
            Some("local".into()),
        )
        .unwrap();
    drop(session);
    let runtime = BackendRuntime::new(database.clone(), vec![provider(config)]).unwrap();
    assert!(!runtime.states()[0].enabled);
    assert!(runtime.accounts("fixture").unwrap().is_empty());
    assert_eq!(runtime.active_account("fixture").unwrap(), None);
    drop(runtime);
    drop(database);
    std::fs::remove_file(path).unwrap();
}

fn config(mode: &str) -> ProcessProviderConfig {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_provider-process-fixture"));
    ProcessProviderConfig {
        package_root: executable.parent().unwrap().to_owned(),
        command: executable.file_name().unwrap().into(),
        args: vec![mode.into()],
        manifest: BackendManifest {
            id: "fixture".into(),
            title: "Fixture".into(),
            version: "1".into(),
            operations: vec![Operation::query("echo", "Echo", Schema::Any, Schema::Any)],
        },
        sources: vec![],
        settings: ExtensionSettings::default(),
        db_path: std::env::temp_dir().join(format!(
            "tradeslope-process-{}-{}.db",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        )),
    }
}

fn provider(config: ProcessProviderConfig) -> Arc<ProcessProvider> {
    Arc::new(
        ProcessProvider::new(config)
            .unwrap()
            .with_timeouts(STARTUP_TIMEOUT, Duration::from_secs(2)),
    )
}

fn session(provider: Arc<ProcessProvider>) -> ProviderSession {
    ProviderSession::new(
        provider,
        Database::in_memory().unwrap(),
        json!({}),
        vec![],
        None,
        None,
    )
    .unwrap()
}

#[test]
fn spawn_context_notifications_persistence_and_restart() {
    let _gate = process_test();
    let config = config("serve");
    let path = config.db_path.clone();
    let provider = provider(config);
    assert!(!provider.default_enabled());
    assert_eq!(provider.manifest().id, "fixture");
    let count = Arc::new(AtomicUsize::new(0));
    let captured = count.clone();
    let session = ProviderSession::new(
        provider.clone(),
        Database::in_memory().unwrap(),
        json!({}),
        vec![],
        None,
        Some(Arc::new(move |_| {
            captured.fetch_add(1, Ordering::SeqCst);
        })),
    )
    .unwrap();
    provider.start(session.services()).unwrap();
    session
        .update(
            json!({}),
            vec![AccountProfile {
                id: "primary".into(),
                label: "Primary".into(),
                env_refs: BTreeMap::from([("key".into(), "ONLY_ENV_REFERENCE".into())]),
            }],
            Some("primary".into()),
        )
        .unwrap();
    let context = provider.execute("echo", json!("context")).unwrap();
    assert_eq!(context["active"], "primary");
    assert_eq!(
        context["accounts"][0]["env_refs"]["key"],
        "ONLY_ENV_REFERENCE"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(provider.execute("echo", json!("persist")).unwrap(), 1);
    provider.stop().unwrap();
    provider.start(session.services()).unwrap();
    assert_eq!(provider.execute("echo", json!("persist")).unwrap(), 2);
    provider.stop().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn invalid_handshakes_are_bounded_and_redacted() {
    let _gate = process_test();
    for mode in ["hang", "log", "wrong-id", "oversize"] {
        let provider = Arc::new(
            ProcessProvider::new(config(mode))
                .unwrap()
                .with_timeouts(Duration::from_millis(500), Duration::from_secs(2)),
        );
        let session = session(provider.clone());
        let start = Instant::now();
        let error = provider.start(session.services()).unwrap_err();
        assert!(!error.to_string().contains("private stdout"));
        assert!(start.elapsed() < Duration::from_secs(3), "{mode}");
        provider.stop().unwrap();
    }
}

#[test]
fn metadata_mismatch_rejected() {
    let _gate = process_test();
    let mut config = config("start-marker");
    let marker = config.db_path.with_extension("start");
    let db_path = config.db_path.clone();
    config.args.push(marker.to_string_lossy().into_owned());
    config.manifest.title = "Unexpected".into();
    let provider = provider(config);
    let session = session(provider.clone());
    assert_eq!(
        provider.start(session.services()).unwrap_err().code,
        "metadata_mismatch"
    );
    assert!(
        !marker.exists(),
        "backend.start ran before metadata acceptance"
    );
    assert!(
        !db_path.exists(),
        "initialize opened the database before metadata acceptance"
    );
}

#[cfg(unix)]
#[test]
fn escaped_descendant_cannot_hold_handshake_stop_or_drop_open() {
    let _gate = process_test();
    struct Detached(PathBuf);
    impl Drop for Detached {
        fn drop(&mut self) {
            if let Ok(pid) = std::fs::read_to_string(&self.0) {
                unsafe {
                    libc::kill(pid.trim().parse().unwrap(), libc::SIGKILL);
                }
                std::fs::remove_file(&self.0).unwrap();
            }
        }
    }
    for mode in ["escape-hang", "escape-serve", "escape-no-read"] {
        let mut config = config(mode);
        let detached = Detached(config.db_path.with_extension("pid"));
        let db_path = config.db_path.clone();
        config.args.push(detached.0.to_string_lossy().into_owned());
        let provider = Arc::new(
            ProcessProvider::new(config)
                .unwrap()
                .with_timeouts(STARTUP_TIMEOUT, Duration::from_secs(2)),
        );
        let session = session(provider.clone());
        let startup = if mode == "escape-hang" {
            let provider = provider.clone();
            let services = session.services();
            Some(std::thread::spawn(move || provider.start(services)))
        } else {
            provider.start(session.services()).unwrap();
            None
        };
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while !detached.0.exists() {
            assert!(Instant::now() < deadline, "descendant never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        // Measure cancellation/teardown, not OS executable validation or startup.
        let start = Instant::now();
        if let Some(startup) = startup {
            session.cancel();
            assert!(startup.join().unwrap().is_err());
        }
        let pid: i32 = std::fs::read_to_string(&detached.0)
            .unwrap()
            .parse()
            .unwrap();
        if mode == "escape-no-read" {
            session
                .update(
                    json!({}),
                    vec![AccountProfile {
                        id: "large".into(),
                        label: "x".repeat(1024 * 1024 - 100),
                        env_refs: BTreeMap::new(),
                    }],
                    None,
                )
                .unwrap();
            let worker = provider.clone();
            let thread = std::thread::spawn(move || worker.execute("echo", Value::Null));
            std::thread::sleep(Duration::from_millis(40));
            provider.stop().unwrap();
            assert!(thread.join().unwrap().is_err());
        }
        drop(session);
        drop(provider);
        assert!(start.elapsed() < Duration::from_secs(3), "{mode}");
        // The retained pipes really are still open in a live escaped process.
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
        drop(detached); // Explicitly clean up the escaped fixture.
        if db_path.exists() {
            std::fs::remove_file(db_path).unwrap();
        }
    }
}

#[test]
fn timeout_cancellation_and_concurrent_stop_interrupt_blocked_io() {
    let _gate = process_test();
    for mode in ["timeout", "cancel", "stop"] {
        let config = config("serve");
        let path = config.db_path.clone();
        let provider = Arc::new(
            ProcessProvider::new(config)
                .unwrap()
                .with_timeouts(STARTUP_TIMEOUT, Duration::from_millis(200)),
        );
        let session = session(provider.clone());
        provider.start(session.services()).unwrap();
        let worker = provider.clone();
        let start = Instant::now();
        let thread = std::thread::spawn(move || worker.execute("echo", json!("block")));
        std::thread::sleep(Duration::from_millis(40));
        match mode {
            "cancel" => session.cancel(),
            "stop" => provider.stop().unwrap(),
            _ => (),
        }
        assert!(thread.join().unwrap().is_err());
        provider.stop().unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn no_spawn_for_metadata_or_bad_paths_and_params_are_bounded() {
    let _gate = process_test();
    let mut config = config("hang");
    config.command = PathBuf::from("../outside");
    let provider = provider(config);
    assert_eq!(provider.sources(), vec![]);
    assert!(
        provider
            .execute("echo", Value::String("x".repeat(1024 * 1024 + 1)))
            .is_err()
    );
    let session = session(provider.clone());
    assert_eq!(
        provider.start(session.services()).unwrap_err().code,
        "invalid_command"
    );
}

#[test]
fn child_not_reading_stdin_cannot_deadlock_stop() {
    let _gate = process_test();
    let provider = provider(config("no-read"));
    let session = session(provider.clone());
    provider.start(session.services()).unwrap();
    session
        .update(
            json!({}),
            vec![AccountProfile {
                id: "large".into(),
                label: "x".repeat(1024 * 1024 - 100),
                env_refs: BTreeMap::new(),
            }],
            None,
        )
        .unwrap();
    let worker = provider.clone();
    let thread =
        std::thread::spawn(move || worker.execute("echo", json!("x".repeat(1024 * 1024 - 2))));
    std::thread::sleep(Duration::from_millis(40));
    let start = Instant::now();
    provider.stop().unwrap();
    assert!(thread.join().unwrap().is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[cfg(unix)]
#[test]
fn drop_reaps_the_child() {
    let _gate = process_test();
    let config = config("serve");
    let path = config.db_path.clone();
    let provider = provider(config);
    let session = session(provider.clone());
    provider.start(session.services()).unwrap();
    let pid = provider
        .execute("echo", json!("pid"))
        .unwrap()
        .as_u64()
        .unwrap() as i32;
    drop(session);
    drop(provider);
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn background_sync_updates_service_without_execute() {
    let _gate = process_test();
    let config = config("watch");
    let path = config.db_path.clone();
    let provider = Arc::new(
        ProcessProvider::new(config)
            .unwrap()
            .with_timeouts(STARTUP_TIMEOUT, Duration::from_secs(2))
            .with_background_sync(true),
    );
    let session = session(provider.clone());
    provider.start(session.services()).unwrap();
    session
        .update(
            json!({}),
            vec![AccountProfile {
                id: "background".into(),
                label: "Background".into(),
                env_refs: BTreeMap::new(),
            }],
            Some("background".into()),
        )
        .unwrap();
    let database = Database::open(&path).unwrap();
    let store = database.namespace("fixture").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while store.get("observed_account").unwrap() != Some(json!("background")) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    provider.stop().unwrap();
    drop(store);
    drop(database);
    std::fs::remove_file(path).unwrap();
}
