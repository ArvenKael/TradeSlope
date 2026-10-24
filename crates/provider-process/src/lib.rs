//! Trusted, explicitly installed executable providers, not a native ABI or sandbox.
//! Stdout is exclusively versioned JSONL; diagnostics belong on inherited stderr.
//! Permissions remain the host's responsibility. Requests are never retried.

mod pipe;
pub mod protocol;
mod server;
pub use server::serve;

use protocol::*;
use serde_json::Value;
use std::{
    io::{BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tradeslope_sdk::runtime::{BackendServices, ExtensionSettings};
use tradeslope_sdk::{Backend, BackendManifest, BackendResult, DataSource};

#[derive(Clone)]
pub struct ProcessProviderConfig {
    pub package_root: PathBuf,
    pub manifest: BackendManifest,
    pub sources: Vec<DataSource>,
    pub settings: ExtensionSettings,
    /// Relative executable, resolved and confined beneath package_root.
    pub command: PathBuf,
    pub args: Vec<String>,
    /// Application database, not an extension-specific database.
    pub db_path: PathBuf,
}

pub struct ProcessProvider {
    config: ProcessProviderConfig,
    connection: Mutex<Option<Arc<Connection>>>,
    lifecycle: Mutex<()>,
    handshake_timeout: Duration,
    execute_timeout: Duration,
    background_sync: bool,
}

struct Connection {
    child: Mutex<Child>,
    stopped: Arc<AtomicBool>,
    // The RPC lock serializes context + execute, never protects child termination.
    rpc: Mutex<RpcState>,
    writer: mpsc::SyncSender<Vec<u8>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    services: BackendServices,
}

struct RpcState {
    id: u64,
    responses: mpsc::Receiver<BackendResult<Frame>>,
    last_context: Option<Context>,
}

impl Connection {
    fn sync_context(
        &self,
        rpc: &mut RpcState,
        context: Context,
        timeout: Duration,
    ) -> BackendResult<()> {
        if rpc.last_context.as_ref() != Some(&context) {
            self.call(rpc, Method::Context(context), timeout)?;
        }
        Ok(())
    }

    fn terminate(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(mut child) = self.child.lock() {
            #[cfg(unix)]
            unsafe {
                // Best-effort descendant cleanup. Cancellable pipes separately
                // guarantee joins even when descendants escape this group.
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn join(&self) {
        if let Ok(mut threads) = self.threads.lock() {
            for thread in threads.drain(..) {
                let _ = thread.join();
            }
        }
    }

    fn call(&self, rpc: &mut RpcState, method: Method, timeout: Duration) -> BackendResult<Value> {
        if self.stopped.load(Ordering::Acquire) || self.services.cancellation.is_cancelled() {
            return Err(failure("process_cancelled"));
        }
        rpc.id = rpc.id.checked_add(1).ok_or_else(|| failure("invalid_id"))?;
        let acknowledged_context = match &method {
            Method::Initialize { context, .. } | Method::Context(context) => Some(context.clone()),
            _ => None,
        };
        let bytes = encode(
            &Request {
                version: VERSION,
                id: rpc.id,
                method,
            },
            MAX_REQUEST_BYTES,
        )?;
        self.writer
            .try_send(bytes)
            .map_err(|_| failure("process_io"))?;
        let deadline = Instant::now() + timeout;
        loop {
            let error = if self.services.cancellation.is_cancelled()
                || self.stopped.load(Ordering::Acquire)
            {
                Some(failure("process_cancelled"))
            } else if Instant::now() >= deadline {
                Some(failure("process_timeout"))
            } else {
                None
            };
            if let Some(error) = error {
                self.terminate();
                return Err(error);
            }
            match rpc.responses.recv_timeout(Duration::from_millis(10)) {
                Ok(Ok(Frame::Response {
                    version,
                    id,
                    result,
                })) if version == VERSION && id == rpc.id => {
                    // Provider-defined error text may contain secrets.
                    let value = result.map_err(|_| failure("provider_error"))?;
                    if let Some(context) = acknowledged_context {
                        if !value.is_null() {
                            self.terminate();
                            return Err(failure("invalid_response"));
                        }
                        rpc.last_context = Some(context);
                    }
                    return Ok(value);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                _ => {
                    self.terminate();
                    return Err(failure("invalid_response"));
                }
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.terminate();
        self.join();
    }
}

impl ProcessProvider {
    pub fn new(mut config: ProcessProviderConfig) -> BackendResult<Self> {
        // Registry-wide operation/source validation belongs to BackendHost.
        if config.manifest.id.trim().is_empty() || config.settings.schema.fields().is_none() {
            return Err(failure("invalid_metadata"));
        }
        config
            .settings
            .schema
            .validate(&config.settings.defaults)
            .map_err(|_| failure("invalid_settings"))?;
        if config.command.is_absolute() || config.command.as_os_str().is_empty() {
            return Err(failure("invalid_command"));
        }
        // Child cwd is the package root, not the application's launch directory.
        config.db_path =
            std::path::absolute(&config.db_path).map_err(|_| failure("invalid_database_path"))?;
        Ok(Self {
            config,
            connection: Mutex::new(None),
            lifecycle: Mutex::new(()),
            handshake_timeout: Duration::from_secs(5),
            execute_timeout: Duration::from_secs(30),
            background_sync: false,
        })
    }

    pub fn with_timeouts(mut self, handshake: Duration, execute: Duration) -> Self {
        self.handshake_timeout = handshake;
        self.execute_timeout = execute;
        self
    }

    /// Opt in for service-only providers which need settings/account changes even
    /// without execute calls. In-flight executions always take priority.
    pub fn with_background_sync(mut self, enabled: bool) -> Self {
        self.background_sync = enabled;
        self
    }

    fn context(services: &BackendServices) -> BackendResult<Context> {
        let (accounts, selected) = services.account_snapshot()?;
        Ok(Context {
            settings: services.settings()?,
            accounts,
            selected,
        })
    }

    fn spawn(&self, services: BackendServices) -> BackendResult<Arc<Connection>> {
        // Fail closed rather than spawn workers with uncancellable platform I/O.
        if !pipe::supported() {
            return Err(failure("process_io_unsupported"));
        }
        let root = self
            .config
            .package_root
            .canonicalize()
            .map_err(|_| failure("invalid_command"))?;
        let executable = root
            .join(&self.config.command)
            .canonicalize()
            .map_err(|_| failure("invalid_command"))?;
        if !executable.starts_with(&root) || !executable.is_file() {
            return Err(failure("invalid_command"));
        }
        let mut command = Command::new(executable);
        command
            .args(&self.config.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|_| failure("process_spawn"))?;
        let stopped = Arc::new(AtomicBool::new(false));
        let pipes = (|| {
            let stdin = child.stdin.take().ok_or_else(|| failure("process_io"))?;
            let stdout = child.stdout.take().ok_or_else(|| failure("process_io"))?;
            Ok((
                pipe::Pipe::new(stdin, stopped.clone()).map_err(|_| failure("process_io"))?,
                pipe::Pipe::new(stdout, stopped.clone()).map_err(|_| failure("process_io"))?,
            ))
        })();
        let (mut stdin, stdout) = match pipes {
            Ok(pipes) => pipes,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let (writer, requests) = mpsc::sync_channel::<Vec<u8>>(1);
        let (responses, receiver) = mpsc::sync_channel(1);
        let connection = Arc::new(Connection {
            child: Mutex::new(child),
            stopped,
            rpc: Mutex::new(RpcState {
                id: 0,
                responses: receiver,
                last_context: None,
            }),
            writer,
            threads: Mutex::new(Vec::new()),
            services,
        });
        // Workers never own the connection: drop cannot run on an I/O thread.
        let stopped = connection.stopped.clone();
        let output_services = connection.services.clone();
        let reader = thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let frame = read::<Frame>(&mut stdout, MAX_RESPONSE_BYTES);
                match frame {
                    Ok(Some(Frame::Changed { version: VERSION })) => {
                        let _ = output_services.notify_changed();
                    }
                    other => {
                        let message = match other {
                            Ok(Some(frame)) => Ok(frame),
                            _ => Err(failure("invalid_response")),
                        };
                        let failed = message.is_err();
                        // Never block behind unsolicited stdout.
                        if responses.try_send(message).is_err() || failed {
                            break;
                        }
                    }
                }
            }
        });
        let writer_thread = thread::spawn(move || {
            loop {
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                match requests.recv_timeout(Duration::from_millis(10)) {
                    Ok(bytes) => {
                        if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => (),
                    Err(_) => break,
                }
            }
        });
        *connection
            .threads
            .lock()
            .map_err(|_| failure("process_lock"))? = vec![reader, writer_thread];
        Ok(connection)
    }
}

impl Backend for ProcessProvider {
    fn default_enabled(&self) -> bool {
        false
    }
    fn manifest(&self) -> BackendManifest {
        self.config.manifest.clone()
    }
    fn sources(&self) -> Vec<DataSource> {
        self.config.sources.clone()
    }
    fn settings(&self) -> ExtensionSettings {
        self.config.settings.clone()
    }

    fn start(&self, services: BackendServices) -> BackendResult<()> {
        let _lifecycle = self.lifecycle.lock().map_err(|_| failure("process_lock"))?;
        if self
            .connection
            .lock()
            .map_err(|_| failure("process_lock"))?
            .is_some()
        {
            return Ok(());
        }
        let connection = self.spawn(services)?;
        let result = (|| {
            let deadline = Instant::now() + self.handshake_timeout;
            let mut rpc = connection.rpc.lock().map_err(|_| failure("process_lock"))?;
            let metadata = connection.call(&mut rpc, Method::Metadata, self.handshake_timeout)?;
            let metadata: Metadata =
                serde_json::from_value(metadata).map_err(|_| failure("invalid_metadata"))?;
            if metadata.manifest != self.config.manifest
                || metadata.sources != self.config.sources
                || metadata.settings != self.config.settings
            {
                return Err(failure("metadata_mismatch"));
            }
            connection.call(
                &mut rpc,
                Method::Initialize {
                    db_path: self.config.db_path.clone(),
                    context: Self::context(&connection.services)?,
                },
                deadline.saturating_duration_since(Instant::now()),
            )?;
            Ok(())
        })();
        if result.is_err() {
            connection.terminate();
            connection.join();
        }
        result?;
        if self.background_sync {
            let weak = Arc::downgrade(&connection);
            let stopped = connection.stopped.clone();
            let timeout = self.handshake_timeout;
            let sync = thread::spawn(move || {
                let mut next = Instant::now() + Duration::from_millis(250);
                while !stopped.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(10));
                    if Instant::now() < next {
                        continue;
                    }
                    next = Instant::now() + Duration::from_millis(250);
                    let Some(connection) = weak.upgrade() else {
                        break;
                    };
                    let Ok(mut rpc) = connection.rpc.try_lock() else {
                        continue;
                    };
                    let Ok(context) = Self::context(&connection.services) else {
                        break;
                    };
                    if connection.sync_context(&mut rpc, context, timeout).is_err() {
                        connection.terminate();
                        break;
                    }
                }
            });
            connection
                .threads
                .lock()
                .map_err(|_| failure("process_lock"))?
                .push(sync);
        }
        *self
            .connection
            .lock()
            .map_err(|_| failure("process_lock"))? = Some(connection);
        Ok(())
    }

    fn execute(&self, operation: &str, params: Value) -> BackendResult<Value> {
        if encode(&params, tradeslope_sdk::MAX_PARAMS_BYTES + 1).is_err() {
            return Err(failure("params_too_large"));
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| failure("process_lock"))?
            .clone()
            .ok_or_else(|| failure("process_not_started"))?;
        let mut rpc = connection.rpc.lock().map_err(|_| failure("process_lock"))?;
        connection.call(
            &mut rpc,
            Method::Context(Self::context(&connection.services)?),
            self.execute_timeout,
        )?;
        connection.call(
            &mut rpc,
            Method::Execute {
                operation: operation.into(),
                params,
            },
            self.execute_timeout,
        )
    }

    fn stop(&self) -> BackendResult<()> {
        let _lifecycle = self.lifecycle.lock().map_err(|_| failure("process_lock"))?;
        if let Some(connection) = self
            .connection
            .lock()
            .map_err(|_| failure("process_lock"))?
            .take()
        {
            // Never wait for the RPC mutex: cancellation must interrupt blocked I/O.
            if let Ok(mut rpc) = connection.rpc.try_lock() {
                let _ = connection.call(&mut rpc, Method::Shutdown, Duration::from_millis(200));
            }
            connection.terminate();
            connection.join();
        }
        Ok(())
    }
}

impl Drop for ProcessProvider {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tradeslope_sdk::runtime::{Database, ProviderSession};

    struct MetadataOnly;
    impl Backend for MetadataOnly {
        fn manifest(&self) -> BackendManifest {
            BackendManifest {
                id: "test".into(),
                title: "Test".into(),
                version: "1".into(),
                operations: vec![],
            }
        }
        fn execute(&self, _: &str, _: Value) -> BackendResult<Value> {
            unreachable!()
        }
    }

    #[test]
    fn background_a_foreground_b_background_a_uses_last_acknowledged_context() {
        use std::os::unix::process::CommandExt;
        let session = ProviderSession::new(
            Arc::new(MetadataOnly),
            Database::in_memory().unwrap(),
            serde_json::json!({}),
            vec![],
            None,
            None,
        )
        .unwrap();
        let (writer, requests) = mpsc::sync_channel(1);
        let (replies, responses) = mpsc::sync_channel(1);
        let connection = Connection {
            child: Mutex::new(Command::new("true").process_group(0).spawn().unwrap()),
            stopped: Arc::new(AtomicBool::new(false)),
            rpc: Mutex::new(RpcState {
                id: 0,
                responses,
                last_context: None,
            }),
            writer,
            threads: Mutex::new(vec![]),
            services: session.services(),
        };
        let a = ProcessProvider::context(&session.services()).unwrap();
        let mut b = a.clone();
        b.selected = Some("B".into());
        let mut rpc = connection.rpc.lock().unwrap();
        let ack = |id| {
            replies
                .send(Ok(Frame::Response {
                    version: VERSION,
                    id,
                    result: Ok(Value::Null),
                }))
                .unwrap()
        };
        ack(1);
        connection
            .call(
                &mut rpc,
                Method::Initialize {
                    db_path: PathBuf::from("unused"),
                    context: a.clone(),
                },
                Duration::from_secs(1),
            )
            .unwrap();
        requests.recv().unwrap();
        // Tick A: no request needed. Foreground execution then ACKs B.
        connection
            .sync_context(&mut rpc, a.clone(), Duration::from_secs(1))
            .unwrap();
        assert!(requests.try_recv().is_err());
        ack(2);
        connection
            .call(&mut rpc, Method::Context(b), Duration::from_secs(1))
            .unwrap();
        requests.recv().unwrap();
        // Settings return to A before the next tick; it must send A again.
        ack(3);
        connection
            .sync_context(&mut rpc, a.clone(), Duration::from_secs(1))
            .unwrap();
        let request: Request = serde_json::from_slice(&requests.recv().unwrap()).unwrap();
        assert!(matches!(request.method, Method::Context(context) if context == a));
        assert_eq!(rpc.last_context.as_ref().map(|c| &c.selected), Some(&None));
    }
}
