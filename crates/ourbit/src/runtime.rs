//! Public-only health worker and backend-only account reference resolution.
use crate::{error, transport::Transport};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tradeslope_sdk::{
    BackendResult, Field, Schema,
    runtime::{AccountProfile, BackendServices, ExtensionSettings},
};

const DEFAULT_INTERVAL: u64 = 30;
const MAX_INTERVAL: u64 = 3600;

/// Declarative registration; the SDK renders this schema without a proc macro.
pub(crate) fn settings() -> ExtensionSettings {
    ExtensionSettings {
        schema: Schema::object(BTreeMap::from([(
            "health_interval_seconds".into(),
            Field {
                schema: Schema::Integer,
                required: true,
                description: "Public spot health check interval in seconds (1..=3600). Changes apply while running; no account or trading access.".into(),
                default: Some(json!(DEFAULT_INTERVAL)),
            },
        )])),
        defaults: json!({"health_interval_seconds": DEFAULT_INTERVAL}),
    }
}

pub(crate) fn validate_settings(value: &Value) -> BackendResult<()> {
    settings().schema.validate(value)?;
    interval(value).map(|_| ())
}

/// Resolve one coherent selected profile snapshot per private request. Explicit
/// selection never silently falls back to constructor credentials.
pub(crate) fn resolve_credentials(
    profile: &AccountProfile,
    market: crate::params::MarketKind,
    read_env: impl Fn(&str) -> Option<String>,
) -> BackendResult<crate::Credentials> {
    let invalid = || {
        error(
            "credentials_required",
            "Selected account credentials are unavailable or invalid",
        )
    };
    let futures = market == crate::params::MarketKind::Futures
        && (profile.env_refs.contains_key("futures_api_key")
            || profile.env_refs.contains_key("futures_api_secret"));
    let (key_role, secret_role) = if futures {
        ("futures_api_key", "futures_api_secret")
    } else {
        ("api_key", "api_secret")
    };
    let key = profile
        .env_refs
        .get(key_role)
        .and_then(|name| read_env(name))
        .ok_or_else(invalid)?;
    let secret = profile
        .env_refs
        .get(secret_role)
        .and_then(|name| read_env(name))
        .ok_or_else(invalid)?;
    crate::Credentials::new(key, secret).map_err(|_| invalid())
}

fn interval(value: &Value) -> BackendResult<Duration> {
    value["health_interval_seconds"]
        .as_u64()
        .filter(|n| (1..=MAX_INTERVAL).contains(n))
        .map(Duration::from_secs)
        .ok_or_else(|| {
            error(
                "invalid_settings",
                "health_interval_seconds must be an integer in 1..=3600",
            )
        })
}

#[derive(Default)]
pub(crate) struct Worker(Mutex<Option<Running>>, Mutex<Option<BackendServices>>);

struct Running {
    shutdown: Arc<(Mutex<bool>, Condvar)>,
    join: JoinHandle<BackendResult<()>>,
}

impl Worker {
    // Outer None means never attached (standalone CLI); inner None means the
    // attached runtime explicitly has no account. Retain attachment on stop.
    pub fn active_account(&self) -> BackendResult<Option<Option<AccountProfile>>> {
        let state = self
            .1
            .lock()
            .map_err(|_| error("internal", "Health worker unavailable"))?;
        state
            .as_ref()
            .map(|services| services.active_account())
            .transpose()
    }

    pub fn start(&self, http: Transport, services: BackendServices) -> BackendResult<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| error("internal", "Health worker unavailable"))?;
        if state.is_some() {
            return Ok(());
        }
        let initial_interval = interval(&services.settings()?)?;
        if services.cancellation.is_cancelled() {
            return Err(error("cancelled", "Runtime already cancelled"));
        }
        let shutdown = Arc::new((Mutex::new(false), Condvar::new()));
        let signal = shutdown.clone();
        let mut attachment = self
            .1
            .lock()
            .map_err(|_| error("internal", "Health worker unavailable"))?;
        let worker_services = services.clone();
        let join = thread::Builder::new()
            .name("ourbit-health".into())
            .spawn(move || run(http, worker_services, signal, initial_interval))
            .map_err(|_| error("internal", "Unable to start health worker"))?;
        *state = Some(Running { shutdown, join });
        *attachment = Some(services);
        Ok(())
    }

    pub fn stop(&self) -> BackendResult<()> {
        // Serialize start/stop through the join: a replacement cannot overlap.
        let mut state = self
            .0
            .lock()
            .map_err(|_| error("internal", "Health worker unavailable"))?;
        if let Some(running) = state.take() {
            *running
                .shutdown
                .0
                .lock()
                .map_err(|_| error("internal", "Health worker unavailable"))? = true;
            running.shutdown.1.notify_all();
            running
                .join
                .join()
                .map_err(|_| error("internal", "Health worker panicked"))??;
        }
        Ok(())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn publish(services: &BackendServices, value: Value) -> BackendResult<()> {
    services.store.set("health", &value)?;
    // Notification is a hint, not a reason to abandon the health service.
    let _ = services.notify_changed();
    Ok(())
}

fn run(
    http: Transport,
    services: BackendServices,
    shutdown: Arc<(Mutex<bool>, Condvar)>,
    mut current_interval: Duration,
) -> BackendResult<()> {
    let mut last_check: Option<Instant> = None;
    loop {
        if services.cancellation.is_cancelled()
            || *shutdown
                .0
                .lock()
                .map_err(|_| error("internal", "Health worker unavailable"))?
        {
            break;
        }
        // Defensive fallback for custom hosts that bypass validate_settings.
        // The SDK runtime validates bounds before saving live settings.
        if let Ok(updated) = services.settings().and_then(|v| interval(&v)) {
            current_interval = updated;
        }
        if last_check.is_none_or(|last| last.elapsed() >= current_interval) {
            last_check = Some(Instant::now());
            let result = http.health();
            let checked_at_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            publish(
                &services,
                json!({
                    "running": true,
                    "ok": result.is_ok(),
                    "market": "spot",
                    "checked_at_ms": checked_at_ms,
                    "health_interval_seconds": current_interval.as_secs(),
                    "error_code": result.err().map(|e| e.code),
                }),
            )?;
        }
        let stopped = shutdown
            .0
            .lock()
            .map_err(|_| error("internal", "Health worker unavailable"))?;
        if *stopped {
            break;
        }
        // Local stop wakes immediately; runtime cancellation and live settings
        // are observed within 100ms even during a long configured interval.
        let _ = shutdown
            .1
            .wait_timeout(stopped, Duration::from_millis(100))
            .map_err(|_| error("internal", "Health worker unavailable"))?;
    }
    publish(
        &services,
        json!({"running": false, "ok": false, "market": "spot"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ourbit;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tradeslope_sdk::{
        Backend,
        runtime::{BackendRuntime, Database},
    };

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "health worker did not make progress"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn runtime_health_is_public_live_idempotent_and_joined() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let stall = Arc::new(AtomicBool::new(false));
        let count = requests.clone();
        let done = shutdown.clone();
        let delay_response = stall.clone();
        let fixture = thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut buf = [0; 4096];
                        // The health client deliberately has a short deadline.
                        // An abandoned TCP connection is not an HTTP request.
                        let n = match stream.read(&mut buf) {
                            Ok(0) => continue,
                            Ok(n) => n,
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    std::io::ErrorKind::WouldBlock
                                        | std::io::ErrorKind::TimedOut
                                        | std::io::ErrorKind::ConnectionReset
                                ) =>
                            {
                                continue;
                            }
                            Err(error) => panic!("{error}"),
                        };
                        let request = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                        assert!(request.starts_with("get /api/v3/ping "));
                        assert!(!request.contains("apikey"));
                        assert!(!request.contains("signature"));
                        if delay_response.load(Ordering::SeqCst) {
                            count.fetch_add(1, Ordering::SeqCst);
                            thread::sleep(Duration::from_millis(1500));
                            continue;
                        }
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        );
                        count.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        let backend = Arc::new(
            Ourbit::with_endpoints(
                &origin,
                &origin,
                Some(crate::Credentials::new("constructor-key", "constructor-secret").unwrap()),
                None,
            )
            .unwrap(),
        );
        let database = Database::in_memory().unwrap();
        let store = database.namespace("ourbit").unwrap();
        assert_eq!(
            backend
                .credentials(crate::params::MarketKind::Spot)
                .unwrap()
                .key,
            "constructor-key"
        );
        let mut host = BackendRuntime::new(database.clone(), vec![backend.clone()]).unwrap();
        let worker_id = backend
            .worker
            .0
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .join
            .thread()
            .id();
        host.set_enabled("ourbit", true).unwrap();
        assert_eq!(
            worker_id,
            backend
                .worker
                .0
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .join
                .thread()
                .id()
        );
        host.save_settings("ourbit", json!({"health_interval_seconds": 1}))
            .unwrap();
        wait_until(|| requests.load(Ordering::SeqCst) >= 2);
        wait_until(|| {
            store
                .get("health")
                .unwrap()
                .is_some_and(|v| v["ok"] == true && v["health_interval_seconds"] == 1)
        });
        assert_eq!(
            database.namespace("other").unwrap().get("health").unwrap(),
            None
        );
        host.save_accounts(
            "ourbit",
            vec![AccountProfile {
                id: "missing".into(),
                label: "No references".into(),
                env_refs: BTreeMap::new(),
            }],
        )
        .unwrap();
        host.select_account("ourbit", Some("missing".into()))
            .unwrap();
        let failure = backend
            .execute("balances", json!({"market": "spot"}))
            .unwrap_err();
        assert_eq!(failure.code, "credentials_required");
        assert_eq!(
            failure.message,
            "Selected account credentials are unavailable or invalid"
        );
        host.select_account("ourbit", None).unwrap();
        assert_eq!(
            backend
                .credentials(crate::params::MarketKind::Spot)
                .unwrap_err()
                .code,
            "credentials_required"
        );
        let before = Instant::now();
        host.set_enabled("ourbit", false).unwrap();
        backend.stop().unwrap();
        assert!(
            backend
                .credentials(crate::params::MarketKind::Spot)
                .is_err()
        );
        assert!(before.elapsed() < Duration::from_secs(2));
        assert!(backend.worker.0.lock().unwrap().is_none());
        assert_eq!(store.get("health").unwrap().unwrap()["running"], false);
        let stopped_count = requests.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(1200));
        assert_eq!(requests.load(Ordering::SeqCst), stopped_count);
        stall.store(true, Ordering::SeqCst);
        host.set_enabled("ourbit", true).unwrap();
        wait_until(|| requests.load(Ordering::SeqCst) > stopped_count);
        let before = Instant::now();
        drop(host);
        assert!(before.elapsed() < Duration::from_millis(1400));
        assert!(backend.worker.0.lock().unwrap().is_none());
        shutdown.store(true, Ordering::SeqCst);
        fixture.join().unwrap();
    }

    #[test]
    fn selected_profile_resolves_only_backend_credentials_without_fallback() {
        use crate::params::MarketKind;
        let mut profile = AccountProfile {
            id: "fixture".into(),
            label: "Fixture".into(),
            env_refs: BTreeMap::from([
                ("api_key".into(), "FIXTURE_KEY".into()),
                ("api_secret".into(), "FIXTURE_SECRET".into()),
            ]),
        };
        let read = |name: &str| match name {
            "FIXTURE_KEY" => Some("fixture-key".into()),
            "FIXTURE_SECRET" => Some("fixture-secret".into()),
            "FUTURES_KEY" => Some("futures-key".into()),
            "FUTURES_SECRET" => Some("futures-secret".into()),
            _ => None,
        };
        assert_eq!(
            resolve_credentials(&profile, MarketKind::Spot, read)
                .unwrap()
                .key,
            "fixture-key"
        );
        assert_eq!(
            resolve_credentials(&profile, MarketKind::Futures, read)
                .unwrap()
                .key,
            "fixture-key"
        );
        profile
            .env_refs
            .insert("futures_api_key".into(), "FUTURES_KEY".into());
        assert!(resolve_credentials(&profile, MarketKind::Futures, read).is_err());
        profile
            .env_refs
            .insert("futures_api_secret".into(), "FUTURES_SECRET".into());
        assert_eq!(
            resolve_credentials(&profile, MarketKind::Futures, read)
                .unwrap()
                .key,
            "futures-key"
        );
        assert!(resolve_credentials(&profile, MarketKind::Spot, |_| None).is_err());
        assert!(
            resolve_credentials(&profile, MarketKind::Spot, |_| Some("\ninvalid".into())).is_err()
        );
    }

    #[test]
    fn interval_bounds_and_defaults_match_runtime_use() {
        // Constructors do not start network work; exercise the public hook.
        let backend =
            Ourbit::with_endpoints("http://127.0.0.1:1", "http://127.0.0.1:1", None, None).unwrap();
        backend.validate_settings(&settings().defaults).unwrap();
        assert_eq!(
            interval(&settings().defaults).unwrap(),
            Duration::from_secs(DEFAULT_INTERVAL)
        );
        for invalid in [
            json!(0),
            json!(3601),
            json!(-1),
            json!(1.5),
            json!("5"),
            Value::Null,
        ] {
            assert!(
                backend
                    .validate_settings(&json!({"health_interval_seconds": invalid}))
                    .is_err()
            );
        }
        assert_eq!(
            interval(&json!({"health_interval_seconds": 3600})).unwrap(),
            Duration::from_secs(3600)
        );
    }
}
