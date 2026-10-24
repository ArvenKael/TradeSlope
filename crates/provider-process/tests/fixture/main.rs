use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    sync::{Arc, Mutex},
    time::Duration,
};
use tradeslope_sdk::runtime::BackendServices;
use tradeslope_sdk::{Backend, BackendManifest, BackendResult, Operation, Schema};

#[derive(Default)]
struct Fixture(
    Mutex<Option<BackendServices>>,
    Mutex<Option<std::thread::JoinHandle<()>>>,
);
impl Backend for Fixture {
    fn manifest(&self) -> BackendManifest {
        BackendManifest {
            id: "fixture".into(),
            title: "Fixture".into(),
            version: "1".into(),
            operations: vec![Operation::query("echo", "Echo", Schema::Any, Schema::Any)],
        }
    }
    fn start(&self, services: BackendServices) -> BackendResult<()> {
        if std::env::args().nth(1).as_deref() == Some("start-marker") {
            std::fs::write(std::env::args().nth(2).unwrap(), b"started").unwrap();
        }
        if std::env::args().nth(1).as_deref() == Some("watch") {
            let services = services.clone();
            *self.1.lock().unwrap() = Some(std::thread::spawn(move || {
                while !services.cancellation.is_cancelled() {
                    let selected = services.active_account().unwrap().map(|a| a.id);
                    services
                        .store
                        .set("observed_account", &json!(selected))
                        .unwrap();
                    std::thread::sleep(Duration::from_millis(10));
                }
            }));
        }
        *self.0.lock().unwrap() = Some(services);
        Ok(())
    }
    fn stop(&self) -> BackendResult<()> {
        if let Some(worker) = self.1.lock().unwrap().take() {
            worker.join().unwrap();
        }
        Ok(())
    }
    fn execute(&self, _: &str, params: Value) -> BackendResult<Value> {
        let guard = self.0.lock().unwrap();
        let services = guard.as_ref().unwrap();
        if params == "pid" {
            return Ok(json!(std::process::id()));
        }
        if params == "block" {
            std::thread::sleep(Duration::from_secs(60));
        }
        if params == "context" {
            services.notify_changed()?;
            return Ok(json!({
                "settings": services.settings()?,
                "accounts": services.accounts()?,
                "active": services.active_account()?.map(|a| a.id),
            }));
        }
        if params == "persist" {
            let count = services
                .store
                .get("count")?
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                + 1;
            services.store.set("count", &json!(count))?;
            return Ok(json!(count));
        }
        Ok(params)
    }
}

fn main() {
    #[cfg(unix)]
    if matches!(
        std::env::args().nth(1).as_deref(),
        Some("escape-hang" | "escape-serve" | "escape-no-read")
    ) {
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("detached")
            .arg(std::env::args().nth(2).unwrap());
        // SAFETY: the child performs only the async-signal-safe setsid syscall.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        // Reap if this fixture remains alive; the integration test explicitly
        // kills the escaped descendant after forcing this parent to terminate.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    match std::env::args().nth(1).as_deref() {
        Some("detached") => {
            std::fs::write(
                std::env::args().nth(2).unwrap(),
                std::process::id().to_string(),
            )
            .unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
        Some("hang" | "escape-hang") => std::thread::sleep(Duration::from_secs(60)),
        Some("log") => {
            println!("private stdout log");
        }
        Some("oversize") => {
            let mut stdout = io::stdout().lock();
            let chunk = vec![b'x'; 4096];
            for _ in 0..4097 {
                if stdout.write_all(&chunk).is_err() {
                    break;
                }
            }
        }
        Some("wrong-id") => {
            let mut line = String::new();
            io::stdin().lock().read_line(&mut line).unwrap();
            println!(r#"{{"type":"response","version":1,"id":999,"result":{{"Ok":null}}}}"#);
        }
        Some("no-read" | "escape-no-read") => {
            use tradeslope_provider_process::protocol::*;
            let request: Request = read(&mut io::stdin().lock(), MAX_REQUEST_BYTES)
                .unwrap()
                .unwrap();
            let fixture = Fixture::default();
            let result = serde_json::to_value(Metadata {
                manifest: fixture.manifest(),
                sources: fixture.sources(),
                settings: fixture.settings(),
            })
            .unwrap();
            write(
                &mut io::stdout(),
                &Frame::Response {
                    version: VERSION,
                    id: request.id,
                    result: Ok(result),
                },
                MAX_RESPONSE_BYTES,
            )
            .unwrap();
            let request: Request = read(&mut io::stdin().lock(), MAX_REQUEST_BYTES)
                .unwrap()
                .unwrap();
            write(
                &mut io::stdout(),
                &Frame::Response {
                    version: VERSION,
                    id: request.id,
                    result: Ok(Value::Null),
                },
                MAX_RESPONSE_BYTES,
            )
            .unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
        _ => {
            let _ = tradeslope_provider_process::serve(Arc::new(Fixture::default()));
        }
    }
}
