# Backend runtime

`runtime::BackendRuntime` owns backend lifecycle and non-secret configuration.
The existing `BackendHost` remains the bounded execution dispatcher.

## Host setup

1. Open `runtime::Database::open(path)` in the application's data directory.
   The caller creates the directory; opening the database does not.
2. Construct `BackendRuntime::new(database, providers)`, where providers are
   `Vec<Arc<dyn Backend>>`. Newly registered backends default to enabled.
   Construction validates all saved configuration before starting providers.
3. Pass `runtime.backends()` to `BackendHost::new`, **not** the raw providers.
   These wrappers enforce enabled state, including after runtime drop.
4. Optionally install `runtime.set_invalidator(Some(callback))` after building
   the host. The callback receives a backend ID. Capture a weak host/client
   reference to avoid an ownership cycle.
5. Keep the runtime alive as long as its providers should run.

Run construction, enable/disable, and destruction off the UI thread: lifecycle
callbacks and SQLite access are synchronous.

## Backend implementation

The `Backend` trait's optional methods are:

```rust,ignore
fn settings(&self) -> ExtensionSettings;
fn start(&self, services: BackendServices) -> BackendResult<()>;
fn stop(&self) -> BackendResult<()>;
```

Default implementations describe empty settings and perform no lifecycle work.
Return `ExtensionSettings { schema, defaults }` from `settings()`. Existing
`Schema` and `Field` describe settings types, descriptions, and UI metadata.
`defaults` is the complete initial value; `Field::default` is metadata and does
not automatically fill missing values. Saved values are full replacements,
strictly validated against the schema, including nested unknown keys.
Incompatible saved settings fail initialization instead of silently resetting.

`start` should save its services, spawn workers, and return promptly. Workers
poll `services.cancellation.is_cancelled()` and use bounded I/O deadlines.
`stop` must join workers and release resources. Cancellation is signalled
before stop, failed-start cleanup, and runtime destruction. Re-enabling creates
a fresh cancellation signal; older clones remain cancelled.

Lifecycle transitions are serialized by mutable runtime access. Repeated
enable/disable operations are no-ops. Failed starts invoke cleanup and do not
enable dispatch. Stop is attempted once even if it returns an error.
In-flight execution finishes before disable calls stop. Callbacks must not
reenter the runtime.

`services.settings()` and `services.accounts()` return current snapshots.
Saving configuration updates already-issued service handles without restarting
the backend. Workers choose safe points to apply changes.

`services.notify_changed()` emits an ordinary backend refresh hint. It is a
no-op until a host installs an invalidator.

Separately install `runtime.set_resetter(Some(callback))` to discard cached
values and terminate old subscriptions on settings, account, or lifecycle
changes. Installing the hook resets existing providers immediately.
Configuration transitions close dispatch admission, drain in-flight execution,
commit configuration, and invoke the reset hook before resuming dispatch.
A reset-hook failure leaves dispatch closed rather than expose stale data;
already-committed configuration is not rolled back. Reset callbacks must not
synchronously execute a backend or reenter the runtime.

Disable closes admission and signals cancellation **before** waiting for
in-flight calls, allowing executions that await cancellation to finish.

## Storage and accounts

`Database::namespace(id)` returns a cloneable `Store` with JSON
`get(key)`, `set(key, &value)`, and `remove(key)` methods. Namespaces are opaque
strings, not path prefixes. Runtime metadata occupies a separate category from
extension data: writing a data key named `settings` or `enabled` cannot overwrite
runtime configuration. All handles share a synchronized SQLite connection.

Persist account profiles through `save_accounts(id, profiles)`:

```json
{
  "id": "main",
  "label": "Main account",
  "env_refs": {
    "api_key": "OURBIT_API_KEY",
    "api_secret": "OURBIT_API_SECRET"
  }
}
```

Profiles reject unknown serialized fields, empty IDs/labels, duplicate IDs,
and environment references that are not portable variable names. The runtime
never reads environment values. A backend may resolve references in memory
when needed, but must never write resolved credentials into settings, profiles,
extension data, logs, or errors.

Selection is explicit: `runtime.select_account(backend_id, Some(profile_id))`
validates that the profile exists and persists its ID; passing `None` clears
selection. `runtime.active_account(backend_id)` returns the selected ID, while
`services.active_account()` returns a live snapshot of the selected profile.
No profile is selected automatically. Replacing account profiles retains a
still-existing selection; deleting the selected profile clears selection.
Profiles and that selection update are persisted in one SQLite transaction
and published together to live services. Selection changes reset cached data
just like other security-sensitive configuration changes.

This database is **not a secret vault**. Arbitrary JSON values and free-text
labels cannot be classified reliably as secrets. Callers must keep all secrets
out of persistence; the typed account API stores references only.

## Checks

`cargo test -p tradeslope-sdk` includes runtime persistence, namespace and
metadata isolation, strict settings validation, account-reference validation,
enable/disable idempotence, host dispatch gating, failure cleanup, cancellation,
live configuration, and invalidation-hook tests. File-backed tests create
temporary databases beneath the SDK crate's `target/` directory and remove them
on completion; they do not use the system temporary directory.
