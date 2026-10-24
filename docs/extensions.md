# Extending TradeSlope

The host owns pages, card lifetimes, input mappings, subscriptions, and app
settings. Backends advertise typed operations; frontend cards declare input
ports. **The user chooses the source for each port**, not a page-wide provider.
Cards know neither exchange endpoints nor credentials.

The core has no normal dependencies on or built-in registrations for Ourbit,
MarketCard, or OpenAlgo. Providers and JavaScript cards are discovered through
`tradeslope-package-sdk`; native cards remain external source crates linked by
an explicit composition hook, not runtime-loaded binaries.

## Build, discover, inspect, and install

```sh
sh scripts/build-extensions.sh --debug
cargo run --bin tradeapp
```

Release is the script's default (`--release` is also accepted). It builds the
standalone Ourbit executable and stages `dist/extensions/{openalgo,ourbit}` for
the current host platform. OpenAlgo's public assets and licenses are copied from
`extensions/openalgo`; the executable's explicit `--manifest` mode generates
Ourbit's metadata. This build step executes that locally built binary; package
**inspection does not**.

The bundled root is selected in this order:

1. `TRADESLOPE_EXTENSIONS_DIR`, when set (an override, not an additional root).
2. `extensions` beside the app executable, if present.
3. The source workspace's `dist/extensions`, if present.
4. The source workspace's `extensions` root, providing the web card without
   building a provider.

The host also discovers managed packages under `<app-data>/extensions`, where
`<app-data>` is the TradeSlope directory described under Persistence below.
Discovery inspects immediate package subdirectories; duplicate IDs across roots
are rejected rather than shadowed.

In **App settings**, enter an external package folder path, then:

1. **Inspect package** validates `extension.json`, typed metadata, contained
   files, and executable permissions without running code or reading credentials.
2. **Install inspected package** copies it into the managed directory without
   overwriting an existing package. New managed packages are disabled.
3. **Restart** to register the installed package.
4. Choose **Enable trusted provider** explicitly for executable providers.
   For web cards, enable the package and restart to apply the registry change.

Install and web-card enable/disable changes require restart; this is not full
hot reload. A disabled provider is still registered from static metadata and
appears in settings without launching its executable. Fresh executable providers,
including bundled ones, default disabled. Existing backend runtime settings,
accounts, persisted enablement, and start/stop behavior are unchanged.

Only enable trusted providers: subprocesses run with the user's privileges,
filesystem access, and inherited environment, **not in a sandbox**. Launch uses
no shell. Bounded framing, queues, deadlines, and validation constrain the
protocol, not the process's OS capabilities. Pass account environment-variable
references only, never credential values in manifests, args, settings, or protocol
account metadata. Actions are explicit and are never automatically retried.
Executable-provider support is currently Unix-only. Non-Unix execution fails
closed before spawning; package inspection and web-card installation remain
metadata/filesystem operations. Unix pipes are nonblocking and cancellation-aware,
including when detached descendants retain inherited handles.

## Package format

The exact API is `PackageManifest`/`PackageKind` in
`crates/package-sdk/src/lib.rs`. Both kinds use `format_version: 1` and nonempty
`id`, `title`, and string `version` fields. Unknown or kind-inappropriate
top-level fields are rejected.

| `kind` | Kind-specific JSON fields |
| --- | --- |
| `"provider"` | `command`, `args`, `backend`, `sources`, `settings` |
| `"javascript"` | `entry`, `card` |

`command` is a package-relative executable file, not a shell expression or URL;
`args` is an optional array of static strings (default `[]`). `backend` is the
SDK `BackendManifest`; `sources` is an optional `DataSource[]` (default `[]`).
`settings` is optional `ExtensionSettings` with `schema` and `defaults` (default
empty object schema/value). Backend ID must equal package ID. These are static
declarations: inspection validates them using metadata-only SDK registration.

For JavaScript, `entry` names a contained UTF-8 document and `card` is a
`CardManifest` with matching ID, `title`, `renderer: "java_script"`, and typed
`inputs`. See `extensions/openalgo/extension.json` for the complete v1 package
with required bars and optional comparison ports. The older JavaScript-only
`{ "schema_version": 2, "entry": ..., "card": ... }` format is still accepted
and normalized to v1; it is not the new package format.

Paths must stay inside the package; traversal and symlink components are
rejected. Inspection and installation have file-count and byte limits. Native
source packages report rebuild-required rather than loading a Rust/GPUI ABI.

## Contracts and source selection

`tradeslope-sdk` provides `Schema`, Query/Action `Operation` definitions,
`InputPort`, `PortBinding`, and validated `BoundInputs`. Operations declare
parameters, output, and Public/PrivateRead/Trade access independently of kind.
Ports declare kind, parameters, expected output, and requiredness.

The simplified UI below is the normal setup path; it is not a
record of completed whole-UI manual testing. An unbound card offers **Choose
data source**, opening an in-card chooser: select a provider and **Spot** or
**Futures**, then search for an instrument/dataset. A source chip identifies an
existing connection. **+ Add source** first chooses an optional role (for
example, comparison), then its source. Each input is independent.

The card's **⋯** menu offers **Manage sources**, **Disconnect source**,
**Change view**, **Advanced mapping** (developer), and **Remove**. There is no
top-level Connections button/panel or raw-operation popup. Arbitrary typed
operation binding and explicit field renames remain available only in the
in-card advanced developer editor. Normal source setup uses declared metadata,
not inferred operation names or parameter semantics.

Advanced bindings retain the rule that fixed arguments override mapped dynamic
parameters; unmapped optional arguments are omitted. Source bindings instead
keep declared controls dynamic, as described below. Neither path provides
expression transforms, unit conversions, joins, or a nested-path editor.

New cards start unbound. Moving a card preserves mappings; replacing its view
clears them. Applying a mapping validates metadata without executing operations.
Layouts and mappings are session-only. Backend settings/accounts/data persist
separately in the app's SQLite database.

Requests reject undeclared keys recursively; outputs may contain extra fields.
Schemas support primitives, objects, arrays, nullable values, enums, unions,
tuples, and explicit `Any` (which gives up static guarantees for that portion).
Provider-specific constraints still need runtime validation.

### Declarative sources and catalogs

Implement `Backend::sources() -> Vec<DataSource>` to expose ready datasets in
normal source selection. Its default is an empty vector: operations remain
available through advanced binding only. Third-party backends must declare
source metadata to participate in the normal UI; a typed operation alone is
not enough. `client.sources()` returns registration-captured `SourceDescriptor`
envelopes containing `backend`, `backend_title`, and `source`.

Each `DataSource` declares:

- `id` and `title`: stable identity and display name.
- `operation`: a public Query providing the dataset.
- `params`: fixed backend arguments.
- `configuration`: an object schema of user setup fields and their defaults.
- `controls`: backend argument names mapped to card-local frontend argument names.
- Optional `catalog`: a `SourceCatalog` describing searchable choices.

`SourceCatalog` declares `operation`, complete fixed `params`, the destination
configuration `parameter`, and `value_field`/`label_field`. Its public Query must
return an array of objects with those two fields declared as required strings.
The selected row's value is copied verbatim into `parameter`; provider-specific
normalization belongs to the backend. Catalog requests are explicit host
requests, not part of binding construction. Source/catalog metadata is validated
at registration.

`source_binding(&client, &port, backend_id, source_id, configuration)` accepts
only declared setup keys, applies configuration field defaults, checks typed
output compatibility using exact output names, and maps only explicitly declared
controls. It returns an ordinary `PortBinding` without network calls. It removes
control arguments from constants so a fixed descriptor default cannot shadow the
frontend's current value. Renamed outputs require advanced binding, not heuristics.

### OpenAlgo timeframe controls

Source setup has no timeframe field. OpenAlgo owns its timeframe selector; the
hosted initial value is `1h`, with exactly `1m`, `5m`, `15m`, `30m`, `1h`, `4h`,
and `1d` available. The frontend subscribes using its selected interval:

```js
const unsubscribe = TradeSlope.inputs.subscribe("bars", { interval }, (bars, error, state) => {
  // Render value/error/status; replace the subscription when interval changes.
});
```

The selected source's `controls` metadata automatically maps the frontend
`interval` to the backend argument; neither the chart nor the user needs to
wire provider-specific parameters. Source editing hides the chart and pauses
its subscription, then restores the same stable card view without recreation.
Moving/resizing cards or switching pages likewise retains their views; removing
a card or changing its view type releases the old view.

### Chart gestures and retained history

Plot dragging pans horizontally; wheel gestures over the plot or either price
axis zoom time only. At the left boundary the card makes a one-shot input request:

```js
TradeSlope.inputs.request(port, { interval, end: oldest - 1, limit: 200 });
```

`end` and candle `time` are UTC Unix seconds. Sources declare `end`/`limit`
controls as well as `interval`; the frontend has no provider-specific paging code.
Ourbit spot end-only requests infer a start bound to make the exchange return
older candles (implemented and live-verified); futures candles also normalize
to UTC seconds.

Each port retains sorted, timestamp-deduplicated history alongside live snapshots.
Older responses cannot overwrite a newer live correction. Prepends preserve
time-based viewport anchors. The actual cap policy is **10,000 candles per port,
keep newest**, not unlimited scrollback or an oldest-first cache: reaching the
cap stops further historical paging, and later live bars can prune old data.
Remaps/timeframe changes reset the retained window. Empty/no-older responses mark
exhaustion; errors show a visible status and permit a later gesture to retry after
exponential backoff capped at 60 seconds. Live refreshes and prepend-generated
viewport events do not recursively request pages.

## Host-managed subscriptions

Native and JavaScript frontends use the same host input routing. Query
subscriptions deliver changed value/error snapshots; identical backend,
operation, and arguments share work, with per-input output projection. Ourbit's
fallback is shared **five-second REST polling, not WebSocket push**. Frontends
do not own REST refresh timers. `BackendServices.notify_changed()` invalidates
the backend's queries immediately, so changes need not wait for the fallback;
it is a refresh hint, not a raw consumer publish hook.

Actions remain explicit submissions, never subscriptions. Dispatch is bounded
and off the GUI thread. Timeouts or dropped receivers do not cancel a remote
call already executing. Show unbound, loading, error, and stale states honestly;
preserve explicit network and authentication errors inside the card. Never
substitute synthetic prices for failed online requests.

## Native Rust/GPUI cards

Implement `tradeslope_native_sdk::NativeCardFactory`, returning a
`NativeCardInstance` owning a GPUI entity. Its manifest declares input ports,
not backend operations. A custom source-linked host can register a factory via
the `register_native` hook in `src/extensions.rs`, for example:

```rust
registry.register_native(Box::new(tradeslope_market_card::MarketCardFactory))?;
```

`CardContext` supplies stable instance `id` and `inputs: BoundInputs`. Keep the
subscription in the instance, drain it without blocking, and drop/recreate it
when context/bindings change:

```rust
let mut quote = context.inputs.subscribe("quote", serde_json::json!({}))?;
// In the instance's nonblocking update path:
if let Ok(snapshot) = quote.try_recv() {
    // Render snapshot.value, snapshot.error, and snapshot.status.
}
// Dropping quote unsubscribes. No frontend REST refresh timer is needed.
```

For one-shot work use `context.inputs.submit(port, params)`. Yield while
waiting; never block GPUI. See `crates/market-card` and `crates/native-sdk` for
the complete entity/factory integration. MarketCard is not bundled or registered
by the core; only development examples link it. Native crates are trusted,
external source extensions: installation requires rebuilding with matching GPUI
versions/features. Runtime native hot-loading is not claimed; an unstable
Rust/GPUI dynamic-library ABI is not a safe plugin contract.

## JavaScript frontend cards

Load local package subdirectories at startup:

```sh
TRADESLOPE_EXTENSIONS_DIR="$PWD/examples/extensions" cargo run --bin tradeapp
```

Each contains `extension.json` in the v1 package format described above (legacy
JavaScript `schema_version: 2` remains accepted), with `entry` and a card
manifest using `renderer: "java_script"` and typed `inputs`.
The SDK quote example declares a primary quote plus an optional
comparison input: map them independently, even to different compatible
providers. An unbound comparison must not prevent the primary quote rendering.

The host injects `TradeSlope` before package scripts:

```js
const unsubscribe = TradeSlope.inputs.subscribe("quote", {}, (value, error, state) => {
  document.querySelector("#price").textContent =
    error ? error.message : value ? String(value.last) : "Loading…";
});
window.addEventListener("pagehide", unsubscribe, { once: true });
```

`subscribe(port, params, callback)` returns an unsubscribe function. The callback's
third argument contains `status` and `hasValue`. Clear cached display data when
`hasValue` is false, even on an error: an account/configuration reset may be
coalesced into an error without an intermediate loading callback. The host
manages changed snapshots, remaps, and active document lifetimes. Use
`TradeSlope.onContextChange` for theme and connected-state UI; context exposes
`active`, `theme`, and each input's `connected` flag, `revision`, presentation
`label`, and effective `controlValues`/`controlOptions`, not provider configuration.
Fixed control values take precedence over the user's selector; unsupported fixed
values must show an explicit unavailable state, never mislabeled data.
`TradeSlope.inputs.request(port, params)` remains available for
one-shot requests and returns a Promise with structured `code`/`message` errors.
Do not add a frontend refresh interval.

The SDK and host suspend data activity while the card is inactive, even if a
package does not handle context itself. Logical subscriptions resume once with
fresh wire identities. One-shot requests reject while inactive and are never
automatically replayed—especially Actions.

Protocol v2 names inputs, not backends. Document sessions reject old-document
responses; binding revisions invalidate obsolete work. Message sizes, queues,
and timeouts are bounded. There is no public `TradeSlope.backend` API and **no
JavaScript BACKEND runtime yet**: JavaScript packages are frontends using the
same host input SDK as native cards.

Package IDs use lowercase letters, digits, `.`, `-`, and `_` and must be unique.
Resources stay inside the package directory. External navigation, popups, and
direct network access are disabled. Only install trusted packages; these
restrictions are not a comprehensive security sandbox.

## Backend services and settings

Implement `tradeslope_sdk::Backend`: `manifest()` advertises typed operations;
`execute()` performs them. An installable executable calls
`tradeslope_provider_process::serve(Arc<dyn Backend>)`; stdout is exclusively
version-1 JSONL protocol, and diagnostics belong on stderr. Ourbit's entry point
is `crates/ourbit/src/bin/tradeslope-ourbit.rs`; with no arguments it serves the
protocol, and `--manifest` prints package metadata without network requests.
See `crates/provider-process/README.md` for wire format and transport limits.

The generic package loader in `src/extension_manager.rs` creates `ProcessProvider`
adapters from metadata, passes them to
`runtime::BackendRuntime::new(database, backends)`, and constructs `BackendHost`
from `runtime.backends()` so dispatch respects enablement. Starting a provider
checks its actual metadata against the package before execution. No Ourbit
registration or provider-specific branch is needed in core. Operation names need
not match frontend ports.

Global App settings manages packages, backend services/settings, and accounts;
it does not select card sources. `settings()` returns declarative
`runtime::ExtensionSettings`. The host renders
the schema in App settings and validates structural types plus your
`validate_settings()` constraints before saving. `defaults` is the complete
initial JSON value; field defaults are UI metadata, not implicit insertion.
For example, these methods belong inside your `impl Backend`:

```rust
fn settings(&self) -> tradeslope_sdk::runtime::ExtensionSettings {
    use tradeslope_sdk::{Field, Schema};
    tradeslope_sdk::runtime::ExtensionSettings {
        schema: Schema::object(std::collections::BTreeMap::from([(
            "health_interval_seconds".into(),
            Field {
                schema: Schema::Integer,
                required: true,
                description: "Public health check interval in seconds".into(),
                default: Some(serde_json::json!(30)),
            },
        )])),
        defaults: serde_json::json!({"health_interval_seconds": 30}),
    }
}

fn validate_settings(&self, settings: &serde_json::Value)
    -> tradeslope_sdk::BackendResult<()>
{
    if settings["health_interval_seconds"].as_u64().is_none_or(|n| n < 5) {
        return Err(tradeslope_sdk::BackendError::new(
            "invalid_settings", "Health interval must be at least five seconds",
        ));
    }
    Ok(())
}
```

Implement `start(BackendServices)` and `stop()`. Enabled services start at app
startup independently of cards; disabling stops them and rejects new work.
Lifecycle callbacks run off GPUI: `start` must spawn long-running work and
return; `stop` must join it. The runtime signals cancellation before stop,
including failed-start cleanup. Settings updates do not implicitly restart
services: read live settings at safe boundaries.

`BackendServices` provides live `settings()`, `accounts()`, `active_account()`,
a cancellation signal, and a backend-namespaced JSON `Store` atop SQLite.
For example, inside a worker after a public health request succeeds:

```rust
let settings = services.settings()?;
let previous = services.store.get("health")?;
let health = serde_json::json!({"ok": true});
if previous.as_ref() != Some(&health) {
    services.store.set("health", &health)?;
    services.notify_changed()?; // Ask the host to refresh subscribed queries now.
}
// Worker loops must check services.cancellation.is_cancelled().
```

The example omits the request/worker/join implementation; see Ourbit's public
health service for the concrete lifecycle. Store writes alone do not notify
subscribers. Namespace isolation separates extension data from runtime settings
and account metadata; this is JSON storage, not a dbtable or scheduler SDK.
Direct SQL and a schema macro are outside the scope of this workflow.

## Persistence, accounts, and access limits

The app opens `runtime.sqlite3` under the standard TradeSlope app-data directory
(macOS Application Support, Linux XDG data home, Windows LOCALAPPDATA).
`TRADESLOPE_DATA_DIR` overrides the directory. App/backend configuration,
enablement, account profiles/active selection, and extension data persist;
workspace layouts and mappings do not.

Account profiles contain only `id`, `label`, and `env_refs` (role to
environment-variable name). Enter references such as
`api_key=OURBIT_API_KEY, api_secret=OURBIT_API_SECRET`, never credential values.
Only the backend resolves the selected account's references. In app runtime,
`active_account() == None` is public-only: it must not silently load default
environment credentials. Ourbit's standalone CLI/library `from_env()` constructor
still supports its separate environment-based workflow.

Selecting an account does **not** authorize private reads or trading. The GUI
currently grants public access only; private GUI/account operations and trading
remain disabled. No actual private account has been verified. Permission checks
precede enqueueing; JSON parameters cannot grant privileges. Declaring an Action
does not enable it.

The host validates inputs before dispatch and outputs before delivery. Document
units (normalized candle timestamps are UTC seconds) and provider constraints.
Use HTTP deadlines and response-size limits, never automatically retry mutations,
and never expose credentials in errors, settings, or Store data. This is not a
secret vault, unauthenticated server, or arbitrary native-library loader.
JavaScript backend execution, scheduler/dbtable SDKs, and Ourbit WebSocket push
are not implemented.

## Validation scope

Package and process test commands:

```sh
cargo test -p tradeslope-package-sdk
cargo test -p tradeslope-provider-process --features test-fixture
node assets/sdk/backend.test.cjs
node extensions/openalgo/live.test.cjs
cargo run --example openalgo_smoke
cargo run --example backend_smoke
```

The process fixture tests use real deterministic subprocesses, not private
accounts. The native examples use development-only `RustEmbed` support in
`examples/support/openalgo.rs`; the core app does not embed chart assets.
Native chart smoke and Chromium gesture validation have been run in the parent
implementation workflow, and the Ourbit spot history fix was live-verified.
Those checks do not constitute full-root GUI manual acceptance. Source chooser,
install/restart/enable flow, overlays, clipping, card dragging, page switching,
and error presentation still need whole-app manual coverage; no pending full-root
GUI check is represented as passed here. This documentation-only update does
not itself rerun builds, subprocess tests, or graphical checks.
