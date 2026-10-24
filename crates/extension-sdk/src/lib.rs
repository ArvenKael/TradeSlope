//! UI-independent extension contracts and a bounded native backend dispatcher.
//!
//! One worker thread per host serializes backend execution. Cloned clients share
//! that worker and its queue; dropping a host does not invalidate existing clients.
//! Dropping the last client/host closes the queue, allowing the worker to drain
//! accepted requests before exiting. A timeout does not cancel backend execution.
//! Backends must therefore provide their own I/O deadlines.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, mpsc},
    time::Duration,
};

mod bindings;
mod live;
pub mod runtime;
pub mod schema;
pub mod source;
pub use bindings::*;
pub use live::*;
pub use schema::{
    Field, Schema, candle_schema, compatible_output, compatible_parameters, market_schema,
    output_fields, potentially_compatible, suggest_output_mapping, ticker_schema,
};
pub use source::*;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_PARAMS_BYTES: usize = 1024 * 1024;
/// Waiting requests, excluding the one currently executing.
pub const QUEUE_CAPACITY: usize = 64;
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Public,
    PrivateRead,
    Trade,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub title: String,
    pub access: Access,
    pub kind: OperationKind,
    pub parameters: Schema,
    pub output: Schema,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Query,
    Action,
}

impl Operation {
    pub fn query(
        id: impl Into<String>,
        title: impl Into<String>,
        parameters: Schema,
        output: Schema,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            access: Access::Public,
            kind: OperationKind::Query,
            parameters,
            output,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendManifest {
    pub id: String,
    pub title: String,
    pub version: String,
    pub operations: Vec<Operation>,
}

pub trait Backend: Send + Sync + 'static {
    fn manifest(&self) -> BackendManifest;
    fn execute(&self, operation: &str, params: Value) -> BackendResult<Value>;

    /// External executables override this to require explicit user enablement.
    fn default_enabled(&self) -> bool {
        true
    }

    /// Ready datasets for the straightforward source picker. Empty means
    /// advanced operation binding only; the SDK never guesses source semantics.
    fn sources(&self) -> Vec<DataSource> {
        Vec::new()
    }

    /// Declarative settings rendered by the host; never include credential values.
    fn settings(&self) -> runtime::ExtensionSettings {
        runtime::ExtensionSettings::default()
    }

    /// Validate provider-specific constraints beyond structural types.
    fn validate_settings(&self, _settings: &Value) -> BackendResult<()> {
        Ok(())
    }

    /// Start the enabled service independently of any frontend blocks.
    fn start(&self, _services: runtime::BackendServices) -> BackendResult<()> {
        Ok(())
    }

    fn stop(&self) -> BackendResult<()> {
        Ok(())
    }
}

pub type BackendResult<T> = Result<T, BackendError>;

/// SDK errors use stable snake_case codes. Backend-defined errors pass through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendError {
    pub code: String,
    pub message: String,
}

impl BackendError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for BackendError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candle {
    /// Unix timestamp in UTC seconds, not milliseconds.
    pub time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Market {
    pub symbol: String,
    pub base: String,
    pub quote: String,
    pub market: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ticker {
    pub symbol: String,
    pub last: f64,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub volume: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    pub private_read: bool,
    pub trade: bool,
}

impl Permissions {
    fn allows(self, access: Access) -> bool {
        match access {
            Access::Public => true,
            Access::PrivateRead => self.private_read,
            Access::Trade => self.trade,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendRequest {
    pub version: u32,
    pub id: u64,
    pub backend: String,
    pub operation: String,
    pub params: Value,
}

/// The bridge owner assembles this envelope with the original request ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendResponse {
    pub version: u32,
    pub id: u64,
    pub result: Option<Value>,
    pub error: Option<BackendError>,
}

struct RegisteredBackend {
    backend: Arc<dyn Backend>,
    manifest: BackendManifest,
    sources: Vec<DataSource>,
}

struct Registry {
    backends: Vec<RegisteredBackend>,
    indices: HashMap<String, usize>,
}

impl Registry {
    fn manifests(&self) -> Vec<BackendManifest> {
        self.backends.iter().map(|b| b.manifest.clone()).collect()
    }
}

struct Job {
    backend: usize,
    operation: String,
    output: Schema,
    params: Value,
    reply: mpsc::Sender<BackendResult<Value>>,
}

pub struct BackendHost {
    registry: Arc<Registry>,
    sender: mpsc::SyncSender<Job>,
    live: Arc<live::LiveHub>,
}

impl BackendHost {
    pub fn new(backends: Vec<Arc<dyn Backend>>) -> BackendResult<Self> {
        let mut registry = Registry {
            backends: Vec::new(),
            indices: HashMap::new(),
        };
        for backend in backends {
            let manifest = catch_unwind(AssertUnwindSafe(|| backend.manifest()))
                .map_err(|_| internal_error())?;
            validate_field(&manifest.id, "backend ID")?;
            validate_field(&manifest.version, "backend version")?;
            if registry.indices.contains_key(&manifest.id) {
                return Err(BackendError::new(
                    "duplicate_backend",
                    "Duplicate backend ID",
                ));
            }
            let mut operations = HashSet::new();
            for operation in &manifest.operations {
                validate_field(&operation.id, "operation ID")?;
                if !operations.insert(&operation.id) {
                    return Err(BackendError::new(
                        "duplicate_operation",
                        "Duplicate operation ID",
                    ));
                }
            }
            let sources = catch_unwind(AssertUnwindSafe(|| backend.sources()))
                .map_err(|_| internal_error())?;
            source::validate_sources(&manifest, &sources)?;
            registry
                .indices
                .insert(manifest.id.clone(), registry.backends.len());
            registry.backends.push(RegisteredBackend {
                backend,
                manifest,
                sources,
            });
        }
        let registry = Arc::new(registry);
        let worker_registry = registry.clone();
        let (sender, receiver) = mpsc::sync_channel::<Job>(QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("tradeslope-backends".into())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    let backend = &worker_registry.backends[job.backend].backend;
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        backend.execute(&job.operation, job.params)
                    }))
                    .unwrap_or_else(|_| Err(internal_error()))
                    .and_then(|value| {
                        job.output
                            .validate(&value)
                            .map_err(|error| BackendError::new("invalid_output", error.message))?;
                        Ok(value)
                    });
                    // A caller may time out or discard its pending request.
                    let _ = job.reply.send(result);
                }
            })
            .map_err(|_| internal_error())?;
        Ok(Self {
            registry,
            sender,
            live: Arc::new(live::LiveHub::new()),
        })
    }

    pub fn manifests(&self) -> Vec<BackendManifest> {
        self.registry.manifests()
    }

    pub fn client(&self, permissions: Permissions) -> BackendClient {
        BackendClient {
            registry: self.registry.clone(),
            sender: self.sender.clone(),
            permissions,
            live: self.live.clone(),
        }
    }
}

#[derive(Clone)]
pub struct BackendClient {
    registry: Arc<Registry>,
    sender: mpsc::SyncSender<Job>,
    permissions: Permissions,
    live: Arc<live::LiveHub>,
}

impl BackendClient {
    /// Captured, validated registration metadata. No backend execution occurs.
    pub fn sources(&self) -> Vec<SourceDescriptor> {
        self.registry
            .backends
            .iter()
            .flat_map(|backend| {
                backend
                    .sources
                    .iter()
                    .cloned()
                    .map(|source| SourceDescriptor {
                        backend: backend.manifest.id.clone(),
                        backend_title: backend.manifest.title.clone(),
                        source,
                    })
            })
            .collect()
    }

    pub fn manifests(&self) -> Vec<BackendManifest> {
        self.registry.manifests()
    }

    pub fn allows(&self, access: Access) -> bool {
        self.permissions.allows(access)
    }

    /// Validate and enqueue without waiting for queue space or backend execution.
    /// The size limit is on the UTF-8 JSON encoding of `params` alone.
    pub fn submit(
        &self,
        backend: &str,
        operation: &str,
        params: Value,
    ) -> BackendResult<PendingRequest> {
        let (index, operation) = self.validate_request(backend, operation, &params)?;
        let (reply, receiver) = mpsc::channel();
        self.sender
            .try_send(Job {
                backend: index,
                operation: operation.id.clone(),
                output: operation.output.clone(),
                params,
                reply,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    BackendError::new("queue_full", "Backend queue is full")
                }
                mpsc::TrySendError::Disconnected(_) => disconnected_error(),
            })?;
        Ok(PendingRequest { receiver })
    }

    fn validate_request(
        &self,
        backend: &str,
        operation: &str,
        params: &Value,
    ) -> BackendResult<(usize, &Operation)> {
        let index = *self
            .registry
            .indices
            .get(backend)
            .ok_or_else(|| BackendError::new("unknown_backend", "Unknown backend"))?;
        let operation = self.registry.backends[index]
            .manifest
            .operations
            .iter()
            .find(|candidate| candidate.id == operation)
            .ok_or_else(|| BackendError::new("unknown_operation", "Unknown operation"))?;
        if !self.permissions.allows(operation.access) {
            return Err(BackendError::new("permission_denied", "Permission denied"));
        }
        // A counting writer avoids allocating a second potentially large copy.
        if serde_json::to_writer(ParamsSize(0), params).is_err() {
            return Err(BackendError::new(
                "params_too_large",
                "Parameters exceed the 1 MiB JSON limit",
            ));
        }
        operation
            .parameters
            .validate_parameters(params)
            .map_err(|error| BackendError::new("invalid_params", error.message))?;
        Ok((index, operation))
    }

    /// Blocking convenience API. Do not call this method on a GUI thread.
    /// Queue admission remains nonblocking; accepted requests wait at most 30s.
    pub fn call(&self, backend: &str, operation: &str, params: Value) -> BackendResult<Value> {
        self.submit(backend, operation, params)?.wait(CALL_TIMEOUT)
    }

    pub fn submit_request(&self, request: BackendRequest) -> BackendResult<PendingRequest> {
        if request.version != PROTOCOL_VERSION {
            return Err(BackendError::new(
                "unsupported_version",
                "Unsupported backend protocol version",
            ));
        }
        self.submit(&request.backend, &request.operation, request.params)
    }
}

pub struct PendingRequest {
    receiver: mpsc::Receiver<BackendResult<Value>>,
}

impl PendingRequest {
    pub fn try_recv(&self) -> Result<BackendResult<Value>, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }

    fn wait(self, timeout: Duration) -> BackendResult<Value> {
        self.receiver
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    BackendError::new("timeout", "Backend request timed out")
                }
                mpsc::RecvTimeoutError::Disconnected => disconnected_error(),
            })?
    }
}

struct ParamsSize(usize);

impl std::io::Write for ParamsSize {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_PARAMS_BYTES - self.0 {
            return Err(std::io::Error::other("params too large"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn internal_error() -> BackendError {
    BackendError::new("internal_error", "Backend failed internally")
}

fn disconnected_error() -> BackendError {
    BackendError::new("disconnected", "Backend worker disconnected")
}

fn validate_field(value: &str, field: &str) -> BackendResult<()> {
    if value.trim().is_empty() {
        Err(BackendError::new(
            "invalid_manifest",
            format!("Missing {field}"),
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Renderer {
    Native,
    JavaScript,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardManifest {
    pub id: String,
    pub title: String,
    pub renderer: Renderer,
    pub inputs: Vec<InputPort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub id: String,
    pub title: String,
    pub version: String,
    pub cards: Vec<CardManifest>,
}

/// Registration metadata only; native factories belong in a UI-specific crate.
/// Extension IDs and card IDs each occupy a global namespace. Versions must be
/// nonempty; version syntax and compatibility policy belong to the application.
/// Input ports declare schemas, never backend/provider operation references.
pub struct ExtensionRegistry {
    manifests: Vec<ExtensionManifest>,
}

impl ExtensionRegistry {
    pub fn new(manifests: Vec<ExtensionManifest>) -> BackendResult<Self> {
        let mut extensions = HashSet::new();
        let mut cards = HashSet::new();
        for manifest in &manifests {
            validate_field(&manifest.id, "extension ID")?;
            validate_field(&manifest.title, "extension title")?;
            validate_field(&manifest.version, "extension version")?;
            if !extensions.insert(&manifest.id) {
                return Err(BackendError::new(
                    "duplicate_extension",
                    "Duplicate extension ID",
                ));
            }
            for card in &manifest.cards {
                validate_field(&card.id, "card ID")?;
                validate_field(&card.title, "card title")?;
                if !cards.insert(&card.id) {
                    return Err(BackendError::new("duplicate_card", "Duplicate card ID"));
                }
                validate_ports(&card.inputs)?;
            }
        }
        Ok(Self { manifests })
    }

    pub fn manifests(&self) -> Vec<ExtensionManifest> {
        self.manifests.clone()
    }
}

#[cfg(test)]
mod binding_tests;
#[cfg(test)]
mod tests;
