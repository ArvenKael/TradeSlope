# TradeSlope

A general-purpose page builder: pages contain selectable, resizable, movable
cards. Trading tools are the first card category, not a constraint on the
workspace.

## Run

```sh
sh scripts/build-extensions.sh --debug
cargo run --bin tradeapp
```

The build script defaults to release without `--debug`; it stages host-platform
packages in `dist/extensions/openalgo` and `dist/extensions/ourbit`, without
building the app or requiring npm. Enable **Ourbit** explicitly in **App settings**
only if you trust its executable. Use **+** to add a card and choose **OpenAlgo
Charts** under **Change view**. Drag the top bar to rearrange cards and drag
dividers to resize them. Market overview is a development example, not a bundled
core card.

The core has no normal dependencies on or built-in registrations for Ourbit,
MarketCard, or OpenAlgo. It discovers packages through the generic package SDK.
`TRADESLOPE_EXTENSIONS_DIR` overrides the bundled package root; otherwise the app
uses an `extensions` directory beside its executable when present, then
`dist/extensions` in the source workspace when present, else the source
`extensions` root (which provides the OpenAlgo web card without a provider build).
Managed installations in the app-data directory's `extensions` folder are also
discovered. Duplicate IDs are errors, not override precedence.

## Choose a card's data source

An unbound card shows **Choose data source**. It opens a chooser inside that
card: select a provider and **Spot** or **Futures**, then search for an
instrument/dataset. A connected card shows its source in a chip. There is no
top-level **Connections** button or panel.

Use **+ Add source** to choose an optional role, such as **Quote comparison**,
then choose its source. Each source is independent: a comparison can use a
different provider or instrument without changing the primary source.

The card's **⋯** menu offers **Manage sources**, **Disconnect source**,
**Change view**, **Advanced mapping** (developer), and **Remove**. Normal setup
uses declared source metadata and exact matching output fields; arbitrary typed
operation binding and explicit field renames belong only in the card's advanced
developer editor, not a top-level popup.

Bindings belong to individual cards, not pages. Two charts on the same page can
use different providers, instruments, or timeframes. New cards start unbound.
Building a source binding validates metadata without executing the data
operation; catalog searches and subsequent subscriptions do make requests.
Disconnecting a source affects only that source's input.

Public market data requires no API key. Native and JavaScript cards subscribe
to host-managed changed snapshots; they do not schedule their own data refreshes.
Identical backend queries share a five-second REST fallback for Ourbit, **not
WebSocket push**. Backend change notifications can invalidate queries immediately.
Requests run on bounded background workers, not the GPUI thread. Network and
authentication errors remain explicit inside the card; failed online requests
never become synthetic prices.

Ourbit is a standalone `tradeslope-ourbit` provider executable, implemented in
`crates/ourbit/src/bin/tradeslope-ourbit.rs`. It uses
`tradeslope-provider-process::serve` for the SDK's bounded JSONL protocol. See
`crates/ourbit/README.md` for operation coverage, symbol/interval rules,
credential environment variables, signing, permissions, and live-test limits.
The app's frontend clients are public/read-only: no account access, order
submission, cancellation, transfer, or withdrawal is enabled in the GUI.

## App settings and persistence

Global **App settings** manages external packages, backend services, their
declared settings, account profiles, and the active account—not card sources.
To install an external package:

1. Enter the package folder path and choose **Inspect package**. Inspection
   validates metadata and files only; it never executes the provider.
2. Choose **Install inspected package**. Installation copies into the managed
   app-data `extensions` directory, never overwrites an existing package, and
   leaves the new package disabled.
3. Restart the app to register the installed package.
4. For a provider, choose **Enable trusted provider** explicitly. For a web card,
   enable it and restart again to apply the card registry change.

Installation and web-card enable/disable require restart; this is not full
runtime hot reload. Provider settings, accounts, and start/stop lifecycle retain
their existing runtime behavior. Provider metadata is registered at startup even
when its executable is disabled; fresh executable providers default disabled.
Enabled backends start with
the app, independently of cards; Ourbit runs a public health service even with
no cards connected. Disabling a backend stops its service and rejects new work.

The app saves backend enablement, settings, account profiles/selection, and
extension JSON data in `runtime.sqlite3` in the standard app-data directory:

- macOS: `~/Library/Application Support/TradeSlope`
- Linux: `$XDG_DATA_HOME/TradeSlope` (default `~/.local/share/TradeSlope`)
- Windows: `%LOCALAPPDATA%\TradeSlope`

Set `TRADESLOPE_DATA_DIR` to override that directory. Layouts and input mappings
are still session-only; persistent backend configuration does not save a workspace.

Account profiles store labels and environment-variable **references**, never
credential values. For example, enter `api_key=OURBIT_API_KEY,
api_secret=OURBIT_API_SECRET` as references, with the values supplied in the
environment of the app process. Only the backend resolves the selected profile;
no active account (`None`) means public-only runtime behavior, not an implicit
environment fallback. Selecting an account does not enable private GUI access
or trading. No actual private account has been verified. The standalone Ourbit
CLI/library environment constructor remains available separately.

## OpenAlgo Charts card

The card hosts the real OpenAlgo Charts widget, including its controls,
indicators, and drawing tools. Its JavaScript, CSS, and license files live in
`extensions/openalgo`, with a version-1 `extension.json` loaded as an ordinary
JavaScript package. The app no longer embeds chart assets in its executable.
It does not require npm, a development server, a CDN, or an OpenAlgo server at
runtime. See `extensions/openalgo/README.md` for upstream provenance.

Without a source, the hosted chart is unbound, not filled with synthetic prices.
With a source it displays that input's candles. The widget knows only its
`bars` and optional `comparison` inputs: no credentials, exchange URLs, backend
IDs, or provider-specific parameters. Source selection belongs to the in-card chooser.
**+ Add source → Comparison** adds another instrument or provider as a line on
the left price scale. Its errors or source changes do not clear the main price.

Timeframe belongs inside OpenAlgo, not in source setup. The hosted chart starts
at **1h** and offers exactly **1m, 5m, 15m, 30m, 1h, 4h, 1d**. It subscribes to
`bars` with `{ interval }`; declared source control metadata automatically maps
that value to the backend argument. Changing timeframe updates the subscription,
not the source configuration. There is no timeframe setup field. The standalone
asset demo still supports explicitly labeled synthetic data outside the host.
The comparison follows the same chart timeframe.

Drag inside the plot to pan horizontally. Wheel gestures, including over either
price axis, zoom time only, not price. Near the left boundary the frontend requests
older candles with `{ interval, end: oldest - 1, limit: 200 }`, where timestamps
are UTC seconds. Sorted, timestamp-deduplicated history merges with live updates
without a slow historical response overwriting live corrections; prepends preserve
the time viewport anchor. Each input retains at most 10,000 candles, **keeping the
newest** and stopping historical paging at that cap. Empty/no-older pages mark
exhaustion. Failures remain visible; another gesture retries after exponential
backoff (up to 60 seconds), not an automatic paging loop. Ourbit spot end-only
history infers the necessary start bound; that fix has been live-verified.
Futures candles are normalized to UTC seconds too.

The widget follows application light/dark mode. Each chart has a persistent
native view keyed by page and card ID:
resizing, moving, and switching tabs do not recreate it. In-card source editing
hides the chart and pauses its subscription, then restores the same view without
recreation. Closing a card or changing its view type releases it. This is session
state, not saved layouts across app restarts.

## Extension SDKs

- `crates/extension-sdk`: schemas, Query/Action operations, input bindings and
  subscriptions, declarative sources, bounded dispatch, backend lifecycle, and
  SQLite-backed services. `Backend::sources() -> Vec<DataSource>` defaults to
  empty (advanced-only); third-party backends must declare source metadata to
  appear in normal source selection. `client.sources()` returns captured
  descriptors; `source_binding` builds a typed binding without network calls.
- `crates/native-sdk`: actual Rust/GPUI card factories and instances, receiving
  only instance identity and mapped `BoundInputs`.
- `crates/package-sdk`: version-1 provider/JavaScript package inspection,
  discovery, managed installation, and frontend enablement registry.
- `crates/provider-process`: trusted executable provider adapter and JSONL server.
- `crates/market-card`: external source-level native example, linked only by
  development examples, independent of Ourbit.
- `crates/ourbit`: standalone backend/connector executable, independent of GPUI.
- `assets/sdk/backend.js`: JavaScript host input SDK with subscriptions,
  versioned requests, document isolation, limits, and errors; not a JS backend runtime.

Native GPUI extensions are external source-level Rust crates using the native
SDK's factories and the `register_native` composition hook in `src/extensions.rs`.
The core does not register MarketCard. Linking a new native crate requires
rebuilding; runtime GPUI hot-loading is not claimed because Rust/GPUI has no
safe stable plugin ABI.
JavaScript card packages can be loaded at startup without rebuilding:

```sh
TRADESLOPE_EXTENSIONS_DIR="$PWD/examples/extensions" cargo run --bin tradeapp
```

This adds the multi-input **SDK quote example** card: bind its quote and,
optionally, a comparison quote, including a different compatible provider.
A package contains `extension.json` and local HTML/JS assets.
See `docs/extensions.md` for registration, lifecycle, and subscription examples.
Only install packages you trust. Provider subprocesses run with the user's
privileges and inherited environment: **they are not sandboxed**. The adapter
launches directly without a shell and bounds protocol sizes, queues, and deadlines,
but those are not OS isolation. Account metadata carries environment-variable
references only, never credential values; Actions are never automatically retried.
Executable providers currently run only on Unix (validated on macOS).
On non-Unix platforms the adapter refuses to spawn until cancellable pipe I/O
is implemented; installing/inspecting packages does not bypass that restriction.
JavaScript packages cannot request arbitrary
URLs or receive backend credentials. Their declared input ports are checked by
the host, and the client independently enforces public/private/trading access.

## Architecture and platform requirements

- GPUI owns the workspace, card chrome, layout, and dragging.
- `src/page/builder.rs` renders the general-purpose page layout.
- `src/ui/web_cards.rs` owns native webview lifetimes separately from pane data.
- `src/ui/native_cards.rs` owns GPUI extension instances by stable page/card ID.
- `src/ui/settings.rs` and `src/ui/root.rs` provide app/backend settings and
  lifecycle controls; `crates/extension-sdk/src/runtime.rs` owns persistence/services.
- `src/extension_manager.rs` discovers and installs packages; the generic web-card
  host serves their local resources through a restricted custom protocol.
- `examples/support/openalgo.rs` is development-only embedded-asset support;
  `RustEmbed` is a dev dependency, not part of the app's asset loading.
- `gpui-webview` embeds the system web engine. Both it and `gpui-kit` enable
  `gpui-fast` so native charts can be clipped by scroll containers and GPUI
  menus/drag previews can render above them on macOS.

Native integration is validated on macOS (WKWebView). The upstream integration
also supports Linux X11/XWayland with WebKitGTK development libraries; native
Wayland is unsupported. Windows requires WebView2, and upstream native
composition does not yet put GPUI overlays above webviews there. Those platforms
need their own integration validation before being treated as supported.

## Checks

```sh
cargo test --workspace --all-targets
cargo fmt --all -- --check
cargo run --example openalgo_smoke
cargo run --example backend_smoke
cargo run --example package_smoke -- dist/extensions/ourbit
node assets/sdk/backend.test.cjs
node extensions/openalgo/live.test.cjs
cargo test -p tradeslope-package-sdk
cargo test -p tradeslope-provider-process --features test-fixture
```

These are validation commands, not a claim that every check was rerun for this
documentation update. Package/process tests cover metadata inspection,
installation, and real deterministic subprocess protocol behavior without private
account calls. Native chart smoke and Chromium gesture checks have been run in
the parent implementation workflow; full-root GUI manual testing is not claimed.

The smoke example opens a real native window, loads development-only embedded assets, checks
the chart canvas and theme/symbol bridge, resizes the window, hides/shows the
webview, checks retained state, and exits. It needs a graphical desktop session;
headless unit tests cannot validate WKWebView composition.
The backend smoke test additionally renders native and JavaScript blocks against
two deterministic providers, verifies renamed output fields and rejected
undeclared requests, then unmaps and remaps the chart without recreating it.

SQLite JSON storage and backend lifecycle are implemented. A JavaScript backend
runtime, scheduler/dbtable SDKs, Ourbit WebSocket push, and additional OpenAlgo
profile/footprint card packages are not implemented.

Manual integration checklist (not a record of completed whole-UI testing):
choose and change a source inside a card, add/disconnect a comparison independently,
change OpenAlgo timeframe, and confirm source editing pauses/hides then restores
the same chart. Also resize a divider, drag a card over a chart, scroll a
partially clipped chart, open its view picker above chart content, switch pages
while the chart is focused, and close a chart card. Verify network/authentication
failures remain explicit. Chart-body pan/zoom belongs to the webview; card
dragging starts on the GPUI top bar.
