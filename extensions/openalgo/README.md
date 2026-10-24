# OpenAlgo Charts input card

Self-contained browser assets for TradeSlope. No npm install, bundler, CDN,
credentials or trading connection is needed for standalone demo mode. In the app,
this frontend declares a typed **Bars** input; the user maps it using Connections.
The document never selects a provider, names a concrete operation, opens source
connections, or handles credentials.
`index.html` is the entry point. Native/webview setup is intentionally outside
this directory's scope.

## Pin, provenance and license

- Package: **`openalgo-charts@2.6.7`**, the npm registry's `latest` stable version
  when downloaded for this change.
- Metadata: <https://registry.npmjs.org/openalgo-charts/2.6.7>
- Tarball: <https://registry.npmjs.org/openalgo-charts/-/openalgo-charts-2.6.7.tgz>
- Upstream: <https://github.com/marketcalls/openalgo-charts>
- Registry tarball SHA-1: `b99ff7742bc21a338b336ce1b0157c9f9db13cdb`.
- Registry tarball integrity (verified before extraction):
  `sha512-/4YruCB57XufLN7IKMmD51EWbkCtN9XIZH/CgbxqpM/KM/m4PVny9E0mbv9i+rkloFS4xJBRCDKZgQykkzYMag==`
- License: Apache-2.0, Copyright 2026 OpenAlgo. The supplied `LICENSE` and
  `NOTICE` are retained verbatim under `vendor/`. Distribute both with these assets.

The four browser IIFEs are copied **byte-for-byte**, without transpilation,
minification or rewritten headers. They provide the chart engine, full drawing
tier, all built-in indicators, and full widget UI. The official widget IIFE
inlines its deferred features: there are **no dynamic imports**, so ESM modules
and lazy `.mjs` chunks are not required with this distribution. Widget styles
are injected by upstream. Optional trade, WebGL, profile, transform and workspace
tiers are not needed for this demo and are not included. Source maps are not
runtime dependencies and are omitted; the base IIFE's upstream source-map comment
is left intact. Developer tools may request that absent map.

| Runtime vendor artifact | Bytes |
| --- | ---: |
| `openalgo-charts.standalone.js` | 623,729 |
| `openalgo-charts.draw.standalone.js` | 227,590 |
| `openalgo-charts.indicators.standalone.js` | 201,695 |
| `openalgo-charts.widget.standalone.js` | 597,508 |
| **JavaScript total** | **1,650,522** |
| `LICENSE` | 11,338 |
| `NOTICE` | 627 |
| **Vendor total** | **1,662,487** |

`vendor.sha256` records the extracted files' checksums. From this directory run
`shasum -a 256 -c vendor.sha256` to verify them.

## Embedding in wry or another webview

Load `extensions/openalgo/index.html` and preserve sibling paths:

```text
index.html
card.css
card.js
vendor/
  openalgo-charts.standalone.js
  openalgo-charts.draw.standalone.js
  openalgo-charts.indicators.standalone.js
  openalgo-charts.widget.standalone.js
  LICENSE
  NOTICE
```

- **File URL:** navigate to the actual `index.html` file with local sibling
  resource access enabled. Chromium file loading was tested; WKWebView/WebKitGTK
  file-access policies must be configured/verified by the native host.
- **Custom scheme:** serve this same tree under one origin, e.g.
  `tradeslope://localhost/openalgo/index.html`. Resolve relative paths and return
  the file bytes unchanged. Use `text/html; charset=utf-8` for `.html`,
  `text/css; charset=utf-8` for `.css`, and
  `text/javascript; charset=utf-8` for `.js`. Register the scheme with the
  webview's appropriate local/secure origin and resource access settings. Missing
  files should return an error, not an HTML success response.
- **HTML-string loading:** the entry uses relative resources. A plain
  `with_html(include_str!(...))` without a resolvable origin/base will not work.
  Prefer navigation to a file/custom-scheme URL. If HTML-string loading is
  required, the host must supply a matching base URL via the webview API; do not
  rely on the executable's working directory. The page's CSP deliberately
  disallows injected `<base>` elements (`base-uri 'none'`).
- No ES module loader, import map, npm runtime, IPC bridge or external CDN is
  required. Modern JavaScript, Canvas 2D, Promise, ResizeObserver, and normal
  DOM/CSS support are required. Upstream owns its ResizeObserver and DPR
  tracking; CSS flex sizing gives it the chart's available space.
- CSP blocks connections (`connect-src 'none'`), external scripts and objects.
  Inline styles are necessary for upstream's injected widget styles and layout.
  The host must not impose a conflicting stricter style policy. Host evaluation
  of JavaScript should use the webview's native evaluation API, not inline scripts.
- Allocate usable width/height to the webview. Narrow cards use upstream mobile
  controls. The wrapper does not create an artificial minimum width.
- `pagehide` destroys the widget and its observers/listeners. This page does not
  reinitialize after back-forward-cache restoration; navigate/reload it when
  reopening a disposed view.

## Host API

After the document has loaded, use the native evaluate-JavaScript API:

```js
await window.TradeSlopeChart.ready;
window.TradeSlopeChart.setTheme("light"); // "dark" or "light"; no recreation
window.TradeSlopeChart.setSymbol("AAPL"); // standalone demo label only; rejects in host mode
const state = window.TradeSlopeChart.getState(); // upstream JSON-safe WidgetState
// Optional before explicitly removing a view; also automatic on pagehide:
window.TradeSlopeChart.dispose();
```

The exported object also has `version: "2.6.7"`. It becomes available when the
deferred `card.js` executes. `ready` resolves after the widget and initial SMA are
installed (mapped data may still be loading), or rejects on initialization failure. Other methods are synchronous
and throw if called before initialization or after disposal. Theme values and
symbols are validated; symbols must be nonempty strings, matching page settings.
Upstream trims/uppercases symbols. Always JSON-serialize host string values when
constructing evaluation calls; never interpolate untrusted text into JavaScript.
No raw widget, feed API or order callbacks are exposed to the native host.

Only when no `window.TradeSlope` SDK is injected is the card marked **DEMO**.
Its 240 OHLCV bars are generated
deterministically from 2025-01-01 UTC, at daily intervals including weekends, with
a 20-period SMA initially attached. They are not any instrument's market history.
Every demo-mode symbol change (host API or widget controls) restores that same dataset:
upstream clears manually supplied bars on symbol changes. Only the daily interval
can be selected in demo mode to avoid relabeling daily data as intraday data. Built-in indicators,
settings and drawing tools are interactive. No feed, broker, news, quotes,
account, order handling or persistence is configured. The clock/UI can reflect
the real runtime; the generated OHLCV does not.

With an injected SDK the card is an abstract chart, not a source chooser:

- `TradeSlope.context.inputs.bars.connected === true` displays **MAPPED** and
  subscribes with `TradeSlope.inputs.subscribe("bars", { interval }, listener)`.
  The SDK owns refresh scheduling and automatic pause. Results are normalized
  `Candle[]`: UTC Unix seconds in `time`, numeric `open`, `high`, `low`, `close`,
  and `volume`. Empty arrays are valid. The wrapper validates, sorts and
  deduplicates by timestamp (last duplicate wins).
- Without a mapping it displays **UNBOUND**, clears the series, and says
  “Map the Bars input using Connections”. It never silently supplies demo bars.
- The user supplies source parameters in the mapping UI. Requests contain no
  source selection or symbol. Chart timeframes use the abstract input's interval
  control, respecting fixed values and supported options. Source editors are
  disabled or hidden; chart types, styles, drawings and indicators remain available.
- The chart uses the host's source label. **MAPPED** means connected, not necessarily
  network access: an input may be backed by a local database.
- Failed refreshes retain explicitly stale bars. The JSON identity of
  the entire `context.inputs` metadata (including numeric/string revisions)
  invalidates pending results on remapping even when `connected` stays true.
  Pause/resume and disposal also invalidate pending results.

The built-in input manifest is owned by the native host; no separate package
manifest is needed here. `node extensions/openalgo/live.test.cjs` tests remapping,
pause/resume, stale errors, validation, disposal, and standalone-vs-unbound mode
using only Node's built-in `vm`.

### Navigation and retained history

Drag the plot horizontally to pan. Wheel over the plot or either price axis
zooms **time only**, including ctrl-wheel; it does not scale prices or zoom the
document. Toolbar controls keep their own wheel behavior.

Live last-200 snapshots **merge**, never replace retained history. The first
nonempty Bars result fits once; live updates and history loads do not refit.
Each currently mapped source/timeframe retains up to **10,000 candles per port**,
keeping the newest on overflow. Remap, timeframe change, or SDK `hasValue: false`
releases that window; there is no cross-account or inactive-timeframe cache.
Pause invalidates in-flight work without discarding the retained window.

When the visible left edge comes within 30 loaded timeline slots of a source's
oldest candle, navigation submits:

```js
TradeSlope.inputs.request("bars", {
  interval: selectedInterval,
  end: oldestTime - 1, // UTC seconds, exclusive of the oldest retained candle
  limit: 200,
});
```

The host input declares optional direct `end` and `limit` parameters; the source
worker maps them (maximum allowed limit 1000). Optional host `controlOptions`
metadata for those fields is not required to request a page. A compatible mapped
Comparison requests its own history against the same visible demand window and
retains its independent left-axis series. No concrete backend operation is named
in this frontend.

One history request per port/generation can be pending. Empty, identical, or
overlap-only pages stop pagination, as does the retention cap. Failed pages retain
data and display a retry message; another pan/zoom after exponential backoff
(1 second up to 60 seconds) retries. No polling or automatic page-chaining timer
is added. Late replies after remap, timeframe change, pause, SDK reset, or dispose
are ignored. Historical overlaps cannot overwrite newer live corrections.

API details were checked against the pinned vendor bundles: widget
`navigation.mousePan: "horizontal"` enables plot dragging; chart `pan`/`zoom`
events signal demand. `series.update()` follows new bars only at nonnegative
`timeScale.rightOffset`. `series.prependData()` merges but does not anchor the
viewport. The wrapper captures visible endpoint times using
`getVisibleLogicalRange()` and `dataLayer.indexToTimeFloat()`, then restores with
`dataLayer.timeToIndexFloat()` and `timeScale.setVisibleLogicalRange()`. This
accounts for the shared Bars/Comparison timeline without double-shifting.
Upstream wheel handling scales prices over axes, so a non-passive capture handler
on the chart canvas container prevents that path and calls `timeScale.zoomAtX()`.
No vendor files are modified.

## JavaScript input SDK v2

The host injects `assets/sdk/backend.js` before frontend code (the filename is
retained for host asset inclusion, not as a legacy API). Its public API is:

```js
TradeSlope.context; // { active, theme: "dark" | "light", inputs: { bars: { connected, revision? } } }
const stop = TradeSlope.onContextChange(context => { /* immediate subscription */ });
const bars = await TradeSlope.inputs.request("bars", {});
stop();
```

IPC sends exactly `{version: 2, session, id, port, params}`; replies are
`{version: 2, session, id, result, error}`. A per-document nonce isolates replies
across navigation. At most 32 requests can be pending; each times out after
35 seconds. Posting failures release their pending slots. `pagehide` rejects
pending calls, clears timers/listeners, and closes the SDK. There is intentionally
no `TradeSlope.backend` compatibility API. The internal host reply hook remains
`window.__tradeslopeBackendResponse`.

The installable example at `examples/extensions/quote/extension.json` uses
`schema_version: 2` and a required query input `quote`, with empty-object
parameters and a normalized ticker output. Its `symbol` and `last` fields are
required; `bid`, `ask`, and `volume` are also required but accept `null`.
Object schemas use `fields: { name: { schema, required, description, default } }`;
nullable numbers use `{ type: "nullable", inner: { type: "number" } }`.

An initialization/runtime error displays a visible fallback rather than leaving
an unexplained blank card. If `card.js` itself fails to load, the initial loading
message remains visible with local-resource guidance.

## Validation

From the repository root:

```sh
node --check extensions/openalgo/card.js
node assets/sdk/backend.test.cjs
node extensions/openalgo/live.test.cjs
node examples/extensions/quote/quote.test.cjs
(cd extensions/openalgo && shasum -a 256 -c vendor.sha256)
python3 -m http.server 8765 --bind 127.0.0.1 --directory extensions/openalgo
```

Open `http://127.0.0.1:8765/smoke-test.html`; it should show `PASS`. This tests
initialization, theme changes, safe symbol labels, stable widget identity, input
validation, indicator/settings dialogs, drawing rail presence, narrow resizing,
local-only resources and pagehide cleanup. The test page is not loaded by the card.
For automated Chromium validation (with that server already running):

```sh
chromium --headless --disable-gpu --virtual-time-budget=6000 --dump-dom \
  http://127.0.0.1:8765/smoke-test.html
```

The browser smoke page intentionally does not inject the SDK: it exercises
standalone demo compatibility, not host mapping. The real native wry/custom-scheme path and target-platform
WebKit/WKWebView engines still require host-level testing; Chromium is not a
substitute for those checks.

For real Chromium pointer/wheel and mapped-history checks, use an existing
Playwright installation (no project npm dependency is needed):

```sh
PLAYWRIGHT_MODULE=/path/to/playwright CHROMIUM=/path/to/chromium \
  node extensions/openalgo/live.test.cjs --browser
```

This starts a temporary loopback server, drives native browser mouse drag/wheel
input, verifies axis-wheel time zoom and unchanged fixed price range, blocks
ctrl-wheel document zoom, and verifies a requested prepend retains the candle's
pixel coordinate and survives a subsequent live last-200 snapshot. The default
Node test also covers independent-port singleflight/exhaustion, retry backoff,
overlap corrections, stale generations, sorted deduplication, and the retention cap.

## Updating

1. Inspect `https://registry.npmjs.org/openalgo-charts/latest`; explicitly select
   a stable version. Download that version's registry metadata and tarball.
2. Verify tarball SHA-512 against `dist.integrity` before extracting it.
3. Inspect that release's README/types and browser bundles. Confirm load order,
   global names, dependencies and whether the widget IIFE still inlines deferred
   features. Do not swap in an ESM entry without also handling all its chunks.
4. Replace only the four original IIFEs and supplied `LICENSE`/`NOTICE` in
   `vendor/`, unchanged. Preserve the original filenames.
5. Update this pin/provenance/size table, `vendor.sha256`, the host API `version`
   field and smoke-test version assertion. Adapt authored wrapper code separately
   if the upstream API changed.
6. Re-run syntax, hash and browser tests, inspect indicators/drawings and test
   native file/custom-scheme loading before shipping. Do not add credentials or
   live data while updating the demo.
