//! Shared latest-value Query subscriptions.
//!
//! This is a REST polling fallback, not a claim that a provider has a push
//! transport. For example Ourbit REST queries still poll (five seconds by
//! default). Provider change notifications can invalidate queries without
//! waiting for that interval. Consumers cannot publish fabricated snapshots.
//!
//! One worker per distinct active query performs nonblocking dispatch and
//! single-flight response consumption, never backend I/O on the UI thread.
//! Each query retains only its latest value/error, not an event queue. Dropping
//! the last subscription removes the query and wakes its worker to exit.
//! Already accepted backend executions cannot be interrupted: providers must
//! impose their own I/O deadlines, as with ordinary submissions.

use crate::*;
use std::{
    collections::BTreeMap,
    sync::{Condvar, Mutex, Weak},
    time::Instant,
};

pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    Loading,
    Live,
    Error,
}

/// The last changed input value, with the current connection outcome.
/// An error retains the last good value. `Live` means a valid snapshot was
/// obtained, not that the provider uses a streaming/push transport.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputSnapshot {
    pub value: Option<Value>,
    pub error: Option<BackendError>,
    pub status: SubscriptionStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct QueryKey {
    backend: String,
    operation: String,
    args: String,
}

impl QueryKey {
    fn new(backend: &str, operation: &str, args: &Value) -> Self {
        Self {
            backend: backend.into(),
            operation: operation.into(),
            args: canonical(args).to_string(),
        }
    }
}

// Remain order-independent even if a host enables serde_json's preserve_order
// feature through dependency feature unification.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(fields) => {
            let sorted: BTreeMap<_, _> = fields.iter().collect();
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonical(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

pub(crate) struct LiveHub {
    queries: Mutex<HashMap<QueryKey, Weak<Query>>>,
    interval: Mutex<Duration>,
}

impl LiveHub {
    pub(crate) fn new() -> Self {
        Self {
            queries: Mutex::new(HashMap::new()),
            interval: Mutex::new(DEFAULT_POLL_INTERVAL),
        }
    }

    fn invalidate(&self, backend: &str) {
        let queries = self.queries.lock().unwrap();
        for (key, query) in queries.iter() {
            if key.backend == backend
                && let Some(query) = query.upgrade()
            {
                query.refresh();
            }
        }
    }

    fn reset(&self, backend: &str) {
        let queries = self.queries.lock().unwrap();
        for (key, query) in queries.iter() {
            if key.backend == backend
                && let Some(query) = query.upgrade()
            {
                let mut state = query.state.lock().unwrap();
                state.epoch = state.epoch.wrapping_add(1);
                state.value = None;
                state.error = None;
                state.revision = state.revision.wrapping_add(1);
                state.dirty = true;
                query.wake.notify_one();
            }
        }
    }
}

struct Query {
    state: Mutex<QueryState>,
    wake: Condvar,
}

struct QueryState {
    subscribers: usize,
    value: Option<Value>,
    error: Option<BackendError>,
    revision: u64,
    epoch: u64,
    dirty: bool,
    next_poll: Instant,
}

impl Query {
    fn refresh(&self) {
        let mut state = self.state.lock().unwrap();
        state.epoch = state.epoch.wrapping_add(1);
        state.dirty = true;
        self.wake.notify_one();
    }
}

/// A cancellation handle and independent latest-value cursor.
///
/// `try_recv` is nonblocking and coalesces skipped updates. It emits only when
/// the projected value or error/status changes (plus the initial Loading state).
/// Drop cancels this subscriber; no unbounded channels are allocated.
pub struct InputSubscription {
    query: Arc<Query>,
    hub: Arc<LiveHub>,
    key: QueryKey,
    output: Schema,
    fields: BTreeMap<String, String>,
    revision: Option<u64>,
    latest: Option<InputSnapshot>,
}

impl InputSubscription {
    pub fn try_recv(&mut self) -> Result<InputSnapshot, mpsc::TryRecvError> {
        let state = self.query.state.lock().unwrap();
        if self.revision == Some(state.revision) {
            return Err(mpsc::TryRecvError::Empty);
        }
        self.revision = Some(state.revision);
        let projected = state
            .value
            .clone()
            .map(|value| BoundRequest::project(value, &self.output, &self.fields))
            .transpose();
        let (value, error) = match projected {
            Ok(value) => (value, state.error.clone()),
            Err(error) => (None, Some(error)),
        };
        let status = if error.is_some() {
            SubscriptionStatus::Error
        } else if value.is_some() {
            SubscriptionStatus::Live
        } else {
            SubscriptionStatus::Loading
        };
        let snapshot = InputSnapshot {
            value,
            error,
            status,
        };
        if self.latest.as_ref() == Some(&snapshot) {
            return Err(mpsc::TryRecvError::Empty);
        }
        self.latest = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// Request an immediate refresh of the shared query. Refreshes coalesce;
    /// even while invalidated there is at most one accepted request in flight.
    pub fn refresh(&self) {
        self.query.refresh();
    }
}

impl Drop for InputSubscription {
    fn drop(&mut self) {
        // Keep the same lock order as subscribe and invalidate.
        let mut queries = self.hub.queries.lock().unwrap();
        let mut state = self.query.state.lock().unwrap();
        state.subscribers -= 1;
        if state.subscribers == 0 {
            queries.remove(&self.key);
            self.query.wake.notify_one();
        }
    }
}

impl BackendHost {
    /// Refresh active queries after an ordinary provider data-change notice.
    /// Use reset for account/configuration changes or backend disable.
    pub fn invalidate(&self, backend: &str) {
        self.live.invalidate(backend);
    }

    /// Discard cached data before a backend/account/configuration transition.
    pub fn reset(&self, backend: &str) {
        self.live.reset(backend);
    }

    /// Configure fallback cadence for this host. A one-second minimum prevents
    /// accidentally turning REST fallback into a tight request loop.
    pub fn set_poll_interval(&self, interval: Duration) -> BackendResult<()> {
        if interval < Duration::from_secs(1) {
            return Err(BackendError::new(
                "invalid_interval",
                "Polling minimum is one second",
            ));
        }
        *self.live.interval.lock().unwrap() = interval;
        // Wake active queries so the new setting takes effect immediately.
        for manifest in self.manifests() {
            self.invalidate(&manifest.id);
        }
        Ok(())
    }
}

impl BackendClient {
    /// A weak host notification hook suitable for backend runtime services.
    /// It never keeps clients, providers, or subscription workers alive.
    pub fn invalidator(&self) -> Arc<dyn Fn(&str) + Send + Sync> {
        let hub = Arc::downgrade(&self.live);
        Arc::new(move |backend| {
            if let Some(hub) = hub.upgrade() {
                hub.invalidate(backend);
            }
        })
    }

    pub fn invalidate(&self, backend: &str) {
        self.live.invalidate(backend);
    }

    /// Clear cached values immediately and fence all pre-reset responses.
    /// Runtime owners must call this for settings/account changes and disable.
    pub fn reset(&self, backend: &str) {
        self.live.reset(backend);
    }

    /// Weak reset hook for backend runtime lifecycle transitions.
    pub fn resetter(&self) -> Arc<dyn Fn(&str) + Send + Sync> {
        let hub = Arc::downgrade(&self.live);
        Arc::new(move |backend| {
            if let Some(hub) = hub.upgrade() {
                hub.reset(backend);
            }
        })
    }

    fn validate_query(
        &self,
        backend: &str,
        operation: &str,
        args: &Value,
    ) -> BackendResult<(usize, &Operation)> {
        let (index, operation) = self.validate_request(backend, operation, args)?;
        if operation.kind != OperationKind::Query {
            return Err(BackendError::new(
                "not_query",
                "Only Queries support subscriptions",
            ));
        }
        Ok((index, operation))
    }

    pub(crate) fn subscribe_input(
        &self,
        backend: &str,
        operation: &str,
        args: Value,
        output: Schema,
        fields: BTreeMap<String, String>,
    ) -> BackendResult<InputSubscription> {
        self.validate_query(backend, operation, &args)?;
        let key = QueryKey::new(backend, operation, &args);
        let mut queries = self.live.queries.lock().unwrap();
        let query = if let Some(query) = queries.get(&key).and_then(Weak::upgrade) {
            query.state.lock().unwrap().subscribers += 1;
            query
        } else {
            let query = Arc::new(Query {
                state: Mutex::new(QueryState {
                    subscribers: 1,
                    value: None,
                    error: None,
                    revision: 0,
                    epoch: 0,
                    dirty: true,
                    next_poll: Instant::now(),
                }),
                wake: Condvar::new(),
            });
            let worker = query.clone();
            let client = self.clone();
            let backend = backend.to_owned();
            let operation = operation.to_owned();
            std::thread::Builder::new()
                .name("tradeslope-query".into())
                .spawn(move || run(worker, client, backend, operation, args))
                .map_err(|_| internal_error())?;
            queries.insert(key.clone(), Arc::downgrade(&query));
            query
        };
        Ok(InputSubscription {
            query,
            hub: self.live.clone(),
            key,
            output,
            fields,
            revision: None,
            latest: None,
        })
    }
}

fn update(state: &mut QueryState, result: BackendResult<Value>) {
    match result {
        Ok(value) => {
            if state.value.as_ref() != Some(&value) || state.error.is_some() {
                state.value = Some(value);
                state.error = None;
                state.revision = state.revision.wrapping_add(1);
            }
        }
        Err(error) => {
            if state.error.as_ref() != Some(&error) {
                state.error = Some(error);
                state.revision = state.revision.wrapping_add(1);
            }
        }
    }
}

fn run(query: Arc<Query>, client: BackendClient, backend: String, operation: String, args: Value) {
    let mut pending: Option<(PendingRequest, Instant, u64)> = None;
    loop {
        let mut state = query.state.lock().unwrap();
        if state.subscribers == 0 {
            break;
        }
        if let Some((request, started, epoch)) = &pending {
            let result = match request.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(disconnected_error())),
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                if *epoch == state.epoch {
                    update(&mut state, result);
                }
                pending = None;
                state.next_poll = Instant::now() + *client.live.interval.lock().unwrap();
            } else if started.elapsed() >= CALL_TIMEOUT && *epoch == state.epoch {
                update(
                    &mut state,
                    Err(BackendError::new("timeout", "Backend request timed out")),
                );
                // Retain the actual pending request: a timeout cannot cancel
                // execution and must not allow overlapping refresh requests.
            }
        }
        if pending.is_none() && (state.dirty || Instant::now() >= state.next_poll) {
            state.dirty = false;
            match client.submit(&backend, &operation, args.clone()) {
                Ok(request) => pending = Some((request, Instant::now(), state.epoch)),
                Err(error) => {
                    update(&mut state, Err(error));
                    state.next_poll = Instant::now() + *client.live.interval.lock().unwrap();
                }
            }
        }
        let delay = if pending.is_some() {
            Duration::from_millis(20)
        } else {
            state.next_poll.saturating_duration_since(Instant::now())
        };
        drop(query.wake.wait_timeout(state, delay).unwrap());
    }
}

#[cfg(test)]
mod tests;
