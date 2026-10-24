//! Persistent, non-secret backend configuration and cooperative lifecycle.
//!
//! Lifecycle callbacks run synchronously on the caller, never a GUI thread.
//! `start` must spawn any long-running work and return; `stop` must join it.
//! Cancellation is signalled before `stop`, including failed starts and drop.
//! This is not a secret vault: settings and JSON data must contain no secrets.
//! Account credentials are represented only by environment-variable names.

use crate::{Backend, BackendError, BackendManifest, BackendResult, Schema};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn error(code: &str, message: &str) -> BackendError {
    BackendError::new(code, message)
}

fn storage_error(_: impl std::fmt::Display) -> BackendError {
    // SQL errors can include persisted data. Do not expose them to callers.
    error("storage_error", "Persistent backend storage failed")
}

/// A shared SQLite connection. Opening never creates parent directories.
#[derive(Clone)]
pub struct Database(Arc<Mutex<Connection>>);

impl Database {
    pub fn open(path: impl AsRef<Path>) -> BackendResult<Self> {
        Self::from_connection(Connection::open(path).map_err(storage_error)?)
    }

    pub fn in_memory() -> BackendResult<Self> {
        Self::from_connection(Connection::open_in_memory().map_err(storage_error)?)
    }

    fn from_connection(connection: Connection) -> BackendResult<Self> {
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(storage_error)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS sdk_backend_values (
                namespace TEXT NOT NULL,
                category TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (namespace, category, key)
            );",
            )
            .map_err(storage_error)?;
        Ok(Self(Arc::new(Mutex::new(connection))))
    }

    /// Revoke persisted execution consent before publishing a fresh installation.
    /// This cannot enable a backend or change an already running instance.
    pub fn revoke_backend_consent(&self, id: &str) -> BackendResult<()> {
        self.namespace(id)?
            .write("runtime", "enabled", &Value::Bool(false))
    }

    /// Namespaces are opaque identifiers, not path prefixes.
    pub fn namespace(&self, namespace: &str) -> BackendResult<Store> {
        if namespace.trim().is_empty() {
            return Err(error("invalid_namespace", "Namespace must not be empty"));
        }
        Ok(Store {
            database: self.clone(),
            namespace: namespace.to_owned(),
        })
    }
}

/// Extension JSON storage, isolated from other namespaces and runtime metadata.
/// Never put credential values in this store.
#[derive(Clone)]
pub struct Store {
    database: Database,
    namespace: String,
}

impl Store {
    pub fn get(&self, key: &str) -> BackendResult<Option<Value>> {
        self.read("data", key)
    }

    pub fn set(&self, key: &str, value: &Value) -> BackendResult<()> {
        self.write("data", key, value)
    }

    pub fn remove(&self, key: &str) -> BackendResult<()> {
        self.database
            .0
            .lock()
            .map_err(storage_error)?
            .execute(
                "DELETE FROM sdk_backend_values WHERE namespace=?1 AND category='data' AND key=?2",
                params![self.namespace, key],
            )
            .map_err(storage_error)?;
        Ok(())
    }

    fn read(&self, category: &str, key: &str) -> BackendResult<Option<Value>> {
        let text: Option<String> = self.database.0.lock().map_err(storage_error)?
            .query_row(
                "SELECT value FROM sdk_backend_values WHERE namespace=?1 AND category=?2 AND key=?3",
                params![self.namespace, category, key],
                |row| row.get(0),
            ).optional().map_err(storage_error)?;
        text.map(|text| serde_json::from_str(&text).map_err(storage_error))
            .transpose()
    }

    fn write(&self, category: &str, key: &str, value: &Value) -> BackendResult<()> {
        let text = serde_json::to_string(value).map_err(storage_error)?;
        self.database
            .0
            .lock()
            .map_err(storage_error)?
            .execute(
                "INSERT INTO sdk_backend_values(namespace,category,key,value) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(namespace,category,key) DO UPDATE SET value=excluded.value",
                params![self.namespace, category, key, text],
            )
            .map_err(storage_error)?;
        Ok(())
    }
}

/// Reuses schema fields for descriptions, types and field-level UI defaults.
/// `defaults` is the complete initial value; field defaults are metadata only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtensionSettings {
    pub schema: Schema,
    pub defaults: Value,
}

impl Default for ExtensionSettings {
    fn default() -> Self {
        Self {
            schema: Schema::object(BTreeMap::new()),
            defaults: serde_json::json!({}),
        }
    }
}

/// Only labels and environment-variable REFERENCES are persisted.
/// SDK code never reads the environment or serializes resolved credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountProfile {
    pub id: String,
    pub label: String,
    pub env_refs: BTreeMap<String, String>,
}

fn validate_accounts(accounts: &[AccountProfile]) -> BackendResult<()> {
    let mut ids = HashSet::new();
    for account in accounts {
        if account.id.trim().is_empty()
            || account.label.trim().is_empty()
            || !ids.insert(&account.id)
            || account.env_refs.iter().any(|(role, name)| {
                role.trim().is_empty()
                    || name.is_empty()
                    || !name.bytes().enumerate().all(|(i, c)| {
                        c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())
                    })
            })
        {
            return Err(error(
                "invalid_accounts",
                "Invalid account profile or environment reference",
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Default)]
pub struct CancellationSignal(Arc<AtomicBool>);

impl CancellationSignal {
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Live configuration handles. Settings updates do not implicitly restart a
/// provider; workers can read these snapshots at their own safe boundaries.
#[derive(Clone)]
pub struct BackendServices {
    pub store: Store,
    pub cancellation: CancellationSignal,
    settings: Arc<RwLock<Value>>,
    accounts: Arc<RwLock<AccountState>>,
    backend_id: String,
    invalidator: Arc<RwLock<Option<Invalidator>>>,
    resetter: Arc<RwLock<Option<Invalidator>>>,
}

/// Hosts should capture a weak client/hub reference to avoid ownership cycles.
pub type Invalidator = Arc<dyn Fn(&str) + Send + Sync>;

struct AccountState {
    profiles: Vec<AccountProfile>,
    selected: Option<String>,
}

impl BackendServices {
    pub fn settings(&self) -> BackendResult<Value> {
        Ok(self.settings.read().map_err(storage_error)?.clone())
    }

    pub fn accounts(&self) -> BackendResult<Vec<AccountProfile>> {
        Ok(self
            .accounts
            .read()
            .map_err(storage_error)?
            .profiles
            .clone())
    }

    /// Coherent profile/selection snapshot for trusted process synchronization.
    /// Separate `accounts` and `active_account` reads can straddle an update.
    pub fn account_snapshot(&self) -> BackendResult<(Vec<AccountProfile>, Option<String>)> {
        let accounts = self.accounts.read().map_err(storage_error)?;
        Ok((accounts.profiles.clone(), accounts.selected.clone()))
    }

    /// Current selected profile, never resolved credential values.
    pub fn active_account(&self) -> BackendResult<Option<AccountProfile>> {
        let accounts = self.accounts.read().map_err(storage_error)?;
        Ok(accounts
            .profiles
            .iter()
            .find(|profile| Some(&profile.id) == accounts.selected.as_ref())
            .cloned())
    }

    /// Publish a data-change hint without coupling backend workers to a host.
    /// No-op until the host installs an invalidator. No lock is held while it runs.
    pub fn notify_changed(&self) -> BackendResult<()> {
        let invalidator = self.invalidator.read().map_err(storage_error)?.clone();
        if let Some(invalidator) = invalidator {
            catch_unwind(AssertUnwindSafe(|| invalidator(&self.backend_id)))
                .map_err(|_| error("notification_error", "Backend invalidator panicked"))?;
        }
        Ok(())
    }
}

impl BackendServices {
    fn reset(&self) -> BackendResult<()> {
        let resetter = self.resetter.read().map_err(storage_error)?.clone();
        if let Some(resetter) = resetter {
            catch_unwind(AssertUnwindSafe(|| resetter(&self.backend_id)))
                .map_err(|_| error("notification_error", "Backend resetter panicked"))?;
        }
        Ok(())
    }
}

/// Trusted process-runner session. Does not load or mutate persisted runtime
/// configuration or enabled state; the application remains its sole owner.
pub struct ProviderSession {
    backend: Arc<dyn Backend>,
    services: BackendServices,
}

impl ProviderSession {
    pub fn new(
        backend: Arc<dyn Backend>,
        database: Database,
        settings: Value,
        accounts: Vec<AccountProfile>,
        selected: Option<String>,
        notifier: Option<Invalidator>,
    ) -> BackendResult<Self> {
        let services = BackendServices {
            store: database.namespace(&backend.manifest().id)?,
            cancellation: CancellationSignal::default(),
            settings: Arc::new(RwLock::new(Value::Null)),
            accounts: Arc::new(RwLock::new(AccountState {
                profiles: Vec::new(),
                selected: None,
            })),
            backend_id: backend.manifest().id,
            invalidator: Arc::new(RwLock::new(notifier)),
            resetter: Arc::new(RwLock::new(None)),
        };
        let session = Self { backend, services };
        session.update(settings, accounts, selected)?;
        Ok(session)
    }

    pub fn services(&self) -> BackendServices {
        self.services.clone()
    }

    pub fn update(
        &self,
        settings: Value,
        accounts: Vec<AccountProfile>,
        selected: Option<String>,
    ) -> BackendResult<()> {
        self.backend
            .settings()
            .schema
            .validate_parameters(&settings)
            .map_err(|_| error("invalid_settings", "Settings do not match schema"))?;
        self.backend.validate_settings(&settings)?;
        validate_accounts(&accounts)?;
        if selected
            .as_ref()
            .is_some_and(|id| !accounts.iter().any(|a| &a.id == id))
        {
            return Err(error("invalid_accounts", "Selected account does not exist"));
        }
        let mut settings_snapshot = self.services.settings.write().map_err(storage_error)?;
        let mut account_snapshot = self.services.accounts.write().map_err(storage_error)?;
        *settings_snapshot = settings;
        *account_snapshot = AccountState {
            profiles: accounts,
            selected,
        };
        Ok(())
    }

    pub fn cancel(&self) {
        self.services.cancellation.cancel();
    }
}

impl Drop for ProviderSession {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendState {
    pub id: String,
    pub enabled: bool,
    pub running: bool,
}

struct Entry {
    backend: Arc<dyn Backend>,
    manifest: BackendManifest,
    metadata: ExtensionSettings,
    services: BackendServices,
    enabled: bool,
    running: bool,
    gate: Arc<RwLock<bool>>,
    accepting: Arc<AtomicBool>,
}

fn callback(action: impl FnOnce() -> BackendResult<()>) -> BackendResult<()> {
    catch_unwind(AssertUnwindSafe(action)).unwrap_or_else(|_| {
        Err(error(
            "lifecycle_error",
            "Backend lifecycle callback panicked",
        ))
    })
}

impl Entry {
    fn start(&mut self) -> BackendResult<()> {
        if self.running {
            return Ok(());
        }
        self.services.cancellation = CancellationSignal::default();
        if let Err(failure) = callback(|| self.backend.start(self.services.clone())) {
            self.services.cancellation.cancel();
            let _ = callback(|| self.backend.stop());
            return Err(failure);
        }
        self.running = true;
        if let Err(failure) = self.services.reset() {
            let _ = self.stop();
            return Err(failure);
        }
        *self.gate.write().map_err(storage_error)? = true;
        self.accepting.store(true, Ordering::Release);
        Ok(())
    }

    fn stop(&mut self) -> BackendResult<()> {
        // Cancel before draining: an in-flight execute may await cancellation.
        self.accepting.store(false, Ordering::Release);
        self.services.cancellation.cancel();
        *self.gate.write().map_err(storage_error)? = false;
        if !self.running {
            return Ok(());
        }
        let result = callback(|| self.backend.stop());
        // Stop is attempted once, even if the provider reports an error.
        self.running = false;
        let reset = self.services.reset();
        result.and(reset)
    }

    fn configure(&self, action: impl FnOnce() -> BackendResult<()>) -> BackendResult<()> {
        // The write lock queues new reads behind the configuration transition.
        // Do not report an enabled provider as disabled while changing settings.
        let gate = self.gate.write().map_err(storage_error)?;
        action()?;
        // Reset failures leave dispatch closed rather than expose cached data.
        if let Err(failure) = self.services.reset() {
            self.accepting.store(false, Ordering::Release);
            return Err(failure);
        }
        drop(gate);
        Ok(())
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Own one runtime per provider collection. Callbacks must not reenter this
/// runtime. Mutations require `&mut self`, serializing lifecycle transitions.
/// Dropping it disables exported host wrappers and stops active providers.
pub struct BackendRuntime {
    entries: BTreeMap<String, Entry>,
    invalidator: Arc<RwLock<Option<Invalidator>>>,
    resetter: Arc<RwLock<Option<Invalidator>>>,
}

impl BackendRuntime {
    pub fn new(database: Database, backends: Vec<Arc<dyn Backend>>) -> BackendResult<Self> {
        Self::build(database, backends, false).map(|(runtime, _)| runtime)
    }

    /// Keep failed services registered so settings can disable or retry them.
    /// Invalid registration or persisted configuration still fails explicitly.
    pub fn new_resilient(
        database: Database,
        backends: Vec<Arc<dyn Backend>>,
    ) -> BackendResult<(Self, Vec<BackendError>)> {
        Self::build(database, backends, true)
    }

    fn build(
        database: Database,
        backends: Vec<Arc<dyn Backend>>,
        tolerate_start_errors: bool,
    ) -> BackendResult<(Self, Vec<BackendError>)> {
        let mut runtime = Self {
            entries: BTreeMap::new(),
            invalidator: Arc::new(RwLock::new(None)),
            resetter: Arc::new(RwLock::new(None)),
        };
        // Validate and load the entire registry before starting any provider.
        for backend in backends {
            let manifest = catch_unwind(AssertUnwindSafe(|| backend.manifest()))
                .map_err(|_| error("invalid_manifest", "Backend manifest panicked"))?;
            if manifest.id.trim().is_empty() || runtime.entries.contains_key(&manifest.id) {
                return Err(error("invalid_manifest", "Empty or duplicate backend ID"));
            }
            let metadata = catch_unwind(AssertUnwindSafe(|| backend.settings()))
                .map_err(|_| error("invalid_settings", "Backend settings metadata panicked"))?;
            if metadata.schema.fields().is_none() {
                return Err(error(
                    "invalid_settings",
                    "Backend settings must declare an object schema",
                ));
            }
            metadata
                .schema
                .validate_parameters(&metadata.defaults)
                .map_err(|_| error("invalid_settings", "Settings defaults do not match schema"))?;
            callback(|| backend.validate_settings(&metadata.defaults))?;
            let store = database.namespace(&manifest.id)?;
            let settings = store
                .read("runtime", "settings")?
                .unwrap_or_else(|| metadata.defaults.clone());
            metadata
                .schema
                .validate_parameters(&settings)
                .map_err(|_| error("invalid_settings", "Saved settings do not match schema"))?;
            callback(|| backend.validate_settings(&settings))?;
            let enabled = match store.read("runtime", "enabled")? {
                Some(Value::Bool(enabled)) => enabled,
                None => backend.default_enabled(),
                _ => return Err(storage_error("Invalid enabled flag")),
            };
            let accounts = store
                .read("runtime", "accounts")?
                .map(serde_json::from_value::<Vec<AccountProfile>>)
                .transpose()
                .map_err(storage_error)?
                .unwrap_or_default();
            validate_accounts(&accounts)?;
            let selected = store
                .read("runtime", "active_account")?
                .map(serde_json::from_value::<Option<String>>)
                .transpose()
                .map_err(storage_error)?
                .flatten();
            if selected
                .as_ref()
                .is_some_and(|id| !accounts.iter().any(|profile| &profile.id == id))
            {
                return Err(error(
                    "invalid_accounts",
                    "Saved selected account does not exist",
                ));
            }
            let backend_id = manifest.id.clone();
            runtime.entries.insert(
                manifest.id.clone(),
                Entry {
                    backend,
                    manifest,
                    metadata,
                    enabled,
                    running: false,
                    gate: Arc::new(RwLock::new(false)),
                    accepting: Arc::new(AtomicBool::new(false)),
                    services: BackendServices {
                        store,
                        cancellation: CancellationSignal::default(),
                        settings: Arc::new(RwLock::new(settings)),
                        accounts: Arc::new(RwLock::new(AccountState {
                            profiles: accounts,
                            selected,
                        })),
                        backend_id,
                        invalidator: runtime.invalidator.clone(),
                        resetter: runtime.resetter.clone(),
                    },
                },
            );
        }
        let mut errors = Vec::new();
        for entry in runtime.entries.values_mut() {
            if entry.enabled
                && let Err(error) = entry.start()
            {
                if !tolerate_start_errors {
                    return Err(error);
                }
                errors.push(error);
            }
        }
        Ok((runtime, errors))
    }

    fn entry(&self, id: &str) -> BackendResult<&Entry> {
        self.entries
            .get(id)
            .ok_or_else(|| error("unknown_backend", "Unknown backend"))
    }

    pub fn states(&self) -> Vec<BackendState> {
        self.entries
            .values()
            .map(|entry| BackendState {
                id: entry.manifest.id.clone(),
                enabled: entry.enabled,
                running: entry.running,
            })
            .collect()
    }

    pub fn settings_metadata(&self, id: &str) -> BackendResult<ExtensionSettings> {
        Ok(self.entry(id)?.metadata.clone())
    }

    /// Install after constructing the host. Existing service clones immediately
    /// see the hook; passing `None` disconnects it.
    pub fn set_invalidator(&mut self, invalidator: Option<Invalidator>) -> BackendResult<()> {
        *self.invalidator.write().map_err(storage_error)? = invalidator;
        Ok(())
    }

    /// Install a security-sensitive cache/subscription reset hook. Existing
    /// providers reset immediately; use weak captures to avoid ownership cycles.
    pub fn set_resetter(&mut self, resetter: Option<Invalidator>) -> BackendResult<()> {
        *self.resetter.write().map_err(storage_error)? = resetter;
        for entry in self.entries.values() {
            entry.configure(|| Ok(()))?;
        }
        Ok(())
    }

    pub fn settings(&self, id: &str) -> BackendResult<Value> {
        self.entry(id)?.services.settings()
    }

    /// Full replacement, strictly validated, not a merge or implicit restart.
    pub fn save_settings(&mut self, id: &str, settings: Value) -> BackendResult<()> {
        let entry = self.entry(id)?;
        entry
            .metadata
            .schema
            .validate_parameters(&settings)
            .map_err(|_| error("invalid_settings", "Settings do not match schema"))?;
        callback(|| entry.backend.validate_settings(&settings))?;
        entry.configure(|| {
            let mut snapshot = entry.services.settings.write().map_err(storage_error)?;
            entry
                .services
                .store
                .write("runtime", "settings", &settings)?;
            *snapshot = settings;
            drop(snapshot);
            Ok(())
        })
    }

    pub fn accounts(&self, id: &str) -> BackendResult<Vec<AccountProfile>> {
        self.entry(id)?.services.accounts()
    }

    pub fn active_account(&self, id: &str) -> BackendResult<Option<String>> {
        Ok(self
            .entry(id)?
            .services
            .accounts
            .read()
            .map_err(storage_error)?
            .selected
            .clone())
    }

    /// Explicit selection only: no account is selected automatically.
    pub fn select_account(&mut self, id: &str, selected: Option<String>) -> BackendResult<()> {
        let entry = self.entry(id)?;
        entry.configure(|| {
            let mut snapshot = entry.services.accounts.write().map_err(storage_error)?;
            if selected
                .as_ref()
                .is_some_and(|id| !snapshot.profiles.iter().any(|profile| &profile.id == id))
            {
                return Err(error("unknown_account", "Selected account does not exist"));
            }
            if snapshot.selected == selected {
                return Ok(());
            }
            entry.services.store.write(
                "runtime",
                "active_account",
                &serde_json::to_value(&selected).map_err(storage_error)?,
            )?;
            snapshot.selected = selected;
            drop(snapshot);
            Ok(())
        })
    }

    pub fn save_accounts(&mut self, id: &str, accounts: Vec<AccountProfile>) -> BackendResult<()> {
        validate_accounts(&accounts)?;
        let entry = self.entry(id)?;
        entry.configure(|| {
        let mut snapshot = entry.services.accounts.write().map_err(storage_error)?;
        let selected = snapshot
            .selected
            .clone()
            .filter(|id| accounts.iter().any(|profile| &profile.id == id));
        // Persist profiles and selection in one transaction, then publish one
        // coherent live snapshot. Deleting a selected profile cannot leave a
        // dangling reference, even if the process stops between SQL writes.
        let profiles_json = serde_json::to_string(&accounts).map_err(storage_error)?;
        let selected_json = serde_json::to_string(&selected).map_err(storage_error)?;
        {
            let mut connection = entry
                .services
                .store
                .database
                .0
                .lock()
                .map_err(storage_error)?;
            let transaction = connection.transaction().map_err(storage_error)?;
            for (key, value) in [
                ("accounts", profiles_json),
                ("active_account", selected_json),
            ] {
                transaction.execute(
                    "INSERT INTO sdk_backend_values(namespace,category,key,value) VALUES (?1,'runtime',?2,?3)
                     ON CONFLICT(namespace,category,key) DO UPDATE SET value=excluded.value",
                    params![entry.services.store.namespace, key, value],
                ).map_err(storage_error)?;
            }
            transaction.commit().map_err(storage_error)?;
        }
        *snapshot = AccountState {
            profiles: accounts,
            selected,
        };
        drop(snapshot);
        Ok(())
        })
    }

    /// Repeated transitions are no-ops. Failed starts remain disabled. Disable
    /// persists before stopping; a stop failure still leaves dispatch disabled.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> BackendResult<()> {
        let entry = self
            .entries
            .get_mut(id)
            .ok_or_else(|| error("unknown_backend", "Unknown backend"))?;
        if entry.enabled == enabled {
            return Ok(());
        }
        if enabled {
            entry.start()?;
            if let Err(failure) =
                entry
                    .services
                    .store
                    .write("runtime", "enabled", &Value::Bool(true))
            {
                let _ = entry.stop();
                return Err(failure);
            }
            entry.enabled = true;
            Ok(())
        } else {
            entry
                .services
                .store
                .write("runtime", "enabled", &Value::Bool(false))?;
            entry.enabled = false;
            entry.stop()
        }
    }

    /// Pass these wrappers to `BackendHost::new`; do not pass raw providers or
    /// they will bypass enabled-state enforcement. In-flight calls finish
    /// before disable/stop. Existing wrappers reject calls after runtime drop.
    pub fn backends(&self) -> Vec<Arc<dyn Backend>> {
        self.entries
            .values()
            .map(|entry| {
                Arc::new(RuntimeBackend {
                    backend: entry.backend.clone(),
                    manifest: entry.manifest.clone(),
                    gate: entry.gate.clone(),
                    accepting: entry.accepting.clone(),
                }) as Arc<dyn Backend>
            })
            .collect()
    }
}

struct RuntimeBackend {
    backend: Arc<dyn Backend>,
    manifest: BackendManifest,
    gate: Arc<RwLock<bool>>,
    accepting: Arc<AtomicBool>,
}

impl Backend for RuntimeBackend {
    fn manifest(&self) -> BackendManifest {
        self.manifest.clone()
    }

    fn sources(&self) -> Vec<crate::DataSource> {
        self.backend.sources()
    }

    fn execute(&self, operation: &str, params: Value) -> BackendResult<Value> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(error("backend_disabled", "Backend is disabled"));
        }
        let running = self.gate.read().map_err(storage_error)?;
        if !*running || !self.accepting.load(Ordering::Acquire) {
            return Err(error("backend_disabled", "Backend is disabled"));
        }
        self.backend.execute(operation, params)
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
