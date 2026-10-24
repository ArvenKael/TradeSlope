# tradeslope-ourbit

UI-independent, blocking Rust implementation of `tradeslope_sdk::Backend`, ID
`ourbit`. All connector source, configuration documentation, and tests live in
this package. The host owns SDK permission grants and must run blocking calls
off the UI/event-loop thread.

```rust,no_run
use serde_json::json;
use tradeslope_ourbit::Ourbit;
use tradeslope_sdk::Backend;

let backend = Ourbit::from_env()?;
let candles = backend.execute("candles", json!({
    "market": "spot",
    "symbol": "BTC_USDT",
    "interval": "1h",
    "limit": 200
}))?;
# Ok::<(), tradeslope_sdk::BackendError>(())
```

### Standalone SDK client

The `ourbit` example calls through `BackendHost` / `BackendClient`, with no UI
dependency. Nothing runs unless you explicitly invoke it:

```sh
cargo run --manifest-path crates/ourbit/Cargo.toml --example ourbit -- --help
cargo run --manifest-path crates/ourbit/Cargo.toml --example ourbit -- \
  ticker '{"market":"spot","symbol":"BTC_USDT"}'
# Private reads require an explicit permission flag and credentials in environment:
cargo run --manifest-path crates/ourbit/Cargo.toml --example ourbit -- \
  --private-read balances '{"market":"spot"}'
```

All documented operations are available through this client. Mutation calls are
denied unless **both** `--allow-trade` and `--confirm-real-order` are supplied.
These are deliberate opt-ins, not a dry-run mode: such calls submit real requests.
The example prints successful provider payloads to stdout; handle private
financial data accordingly. It never accepts credentials as command-line flags.

## Configuration and safety

### Runtime health service

The SDK runtime calls `Backend::start` independently of cards or GUI polling.
Ourbit spawns one cooperative `ourbit-health` worker; repeated starts do not
spawn duplicates. `Backend::stop` wakes the worker and joins it, and dropping the
backend also stops it. Hosts must call lifecycle methods off the UI thread.

`Backend::settings()` declaratively registers just `health_interval_seconds`:
an integer from 1 through 3600, default **30**. There is no default market
setting: this worker intentionally checks only the public **spot** ping.
The worker reads live settings at least every 100 ms between requests. Startup
rejects out-of-range intervals. `Backend::validate_settings` enforces these
bounds in addition to the generic integer schema; the SDK runtime must call it
before saving live settings. Custom hosts that bypass validation retain the
last valid interval until corrected.

The namespaced public store key `health` contains `running`, `ok`, `market`,
`checked_at_ms`, `health_interval_seconds`, and a sanitized `error_code` (or
null). Shutdown replaces it with a stopped status. Writes issue
`notify_changed()` hints. This is latest status, not a historical log. No account
data, credentials, endpoints, upstream bodies or raw network errors are stored.

Background checks share the existing process-wide rate limiter, but skip rather
than wait behind foreground traffic or provider cooldowns. Each check has one
attempt, no retry, and a **one-second** total request/body deadline. Stopping wakes
idle waits immediately; an in-flight check can take up to that deadline before
the join completes (plus storage/notification work). Synchronous foreground
operations retain the separate deadlines/retry policy below.

When a runtime account is explicitly selected, private backend requests resolve
its `api_key` and `api_secret` environment-variable references in memory per
request. Futures optionally uses `futures_api_key` / `futures_api_secret`; both
must be present when either is supplied, otherwise it uses the common pair.
Missing, unset or invalid selected references fail with a sanitized credential
error: there is **no fallback** to constructor credentials for a selected profile.
For a runtime-attached backend, no selected profile means **public / no account**:
private requests are rejected even if constructor credentials exist. Attachment
is retained after stop. Only standalone backends never attached to runtime
services retain the constructor-credential fallback used by the CLI.
Profile edits and selection are live; resolved secrets are never stored or sent
to the GUI. The health worker never resolves credentials or calls private/order
endpoints. The GUI remains public-only and this integration grants no additional
trading permissions; the host must still explicitly authorize private calls.

- Production origins are fixed: spot `https://api.ourbit.com`, futures
  `https://futures.ourbit.com`. No endpoint environment override is recognized.
- `Ourbit::from_env()` optionally reads `OURBIT_API_KEY` and
  `OURBIT_API_SECRET`. Both or neither must be set. An optional separate pair
  `OURBIT_FUTURES_API_KEY` / `OURBIT_FUTURES_API_SECRET` overrides futures
  credentials; otherwise futures uses the common pair.
- `Ourbit::with_credentials(String, String)` supplies credentials in memory at
  the fixed production origins. It never saves them or updates the environment.
  Supply real secrets through your host's trusted secret input or process
  environment, never through committed files or public JS.
- `Ourbit::with_endpoints(spot, futures, spot_credentials, futures_credentials)`
  is an **explicit constructor** for fixtures/trusted gateways. It accepts only
  HTTPS origins, or numeric-loopback HTTP for TCP fixtures. URL userinfo, paths,
  queries and fragments are rejected. A configured gateway receives credentials:
  do not point a credentialed backend at an untrusted gateway.
- Rustls certificate/hostname verification stays enabled. Redirects, ambient
  proxies and automatic decompression are disabled. Public requests never carry
  credential headers, even on a credentialed instance.
- Connection timeout: 5 seconds. Per-attempt request/body timeout: 15 seconds.
  Response body bound: 8 MiB, enforced on both declared and streamed length.
- Process-wide spacing is 210 ms (under 5 requests/s) across both markets and all
  instances, including private requests and retries. Spot account reads use
  510 ms spacing to respect their documented 2/s limit. Other processes/clients
  sharing the same IP/account must coordinate separately.
- GETs have at most **one retry**, only on transport errors, HTTP 5xx or HTTP
  429. No retry on provider-level error envelopes, malformed responses, 4xx other
  than 429, or redirects. HTTP 418 does not retry. POST and DELETE **never retry**.
- `Retry-After` seconds and HTTP dates establish a shared cooldown. Automatic
  waiting is bounded to two seconds; longer cooldowns return `rate_limited`
  immediately without issuing another request until the cooldown permits it.
  Missing/invalid headers back off for one second. A duration too large for the
  monotonic clock suspends the limiter rather than retrying early.
- No credentials, signed URLs, raw reqwest errors, provider error bodies, or
  upstream messages are included in returned errors or `Debug`.
- A mutation transport/server/parse error is **not proof of rejection**. Reconcile
  against account/order state before any resubmission. There is no idempotency
  guarantee. The SDK's caller timeout does not cancel an in-progress request.
- Direct `Backend::execute` calls are trusted native calls. The SDK manifest
  labels permissions; the host must gate untrusted callers through the SDK.

## Typed manifest contract

Every operation advertises its exact parameter object and result schema through
`Backend::manifest()`. All reads, including signed account reads, are
`OperationKind::Query`; only `place_order`, `cancel_order`, and `cancel_all` are
`OperationKind::Action`. Access permissions are separate from operation kind.
These are connector contracts for user mappings, not UI block/port declarations.
The SDK host validates parameter and output values; parameter objects reject
unknown fields, so mappings must not add unrelated constants.

Field descriptions explain units, bounds and market-dependent rules. String
enums expose market and interval options directly to generic mapping forms.
`Union` preserves spot string order enums versus native futures integers, and
exact decimal strings versus JSON numbers. Order-book levels use fixed
two-number `Tuple` schemas. The execution validator remains authoritative for
numeric bounds, conditional required/inapplicable fields, supported intervals
per market, and order-specific financial rules; the schema does not express
these cross-field constraints.

Defaults are metadata, not automatic parameter injection. `market: "spot"`,
`symbol: "BTC_USDT"` and `interval: "1h"` are suggested form values: execution
still requires them wherever documented. Optional `limit: 200` and futures
`page: 1` match execution defaults; lower endpoint limits still clamp to 100.
Union fields do not invent trading defaults.

Normalized public outputs use SDK candle/market arrays and ticker object schemas,
plus explicit health, book, trade and funding schemas. Ticker `bid`, `ask` and
`volume` are present but nullable: a missing or null provider field becomes JSON
null, not zero. Other required normalized fields are never silently filled.
**`Schema::Any` is deliberate only for signed operation outputs**: these preserve
unstable raw provider payloads (including futures success envelopes). `Any`
does not promise normalized account/order fields or validate their shape;
consumers must understand the provider contract before mapping those values.

## Common parameter rules

Every call requires an object with `market: "spot" | "futures"`. Missing fields,
nulls, wrong JSON types, unknown/inapplicable fields and invalid enum values
are errors, not silently ignored.

`symbol` accepts an actual alphanumeric pair such as `BTCUSDT` or `BTC_USDT`
(case-insensitive), not a placeholder. Spot sends `BTCUSDT`; futures sends
`BTC_USDT`. Compact futures symbols split only on known `USDT`, `USDC`, `USD`,
`BTC`, `ETH` suffixes. Use explicit `BASE_QUOTE` for other quotes.

`limit` is an integer in `1..=1000`, default 200, on operations that accept it.
Lower endpoint limits clamp (futures trades and private page sizes: 100).
`page` is a futures-private page number in `1..=100000`, default 1.
There is no automatic pagination. Explicit operations do not start polling;
the SDK lifecycle health service described above runs only after runtime start.

`start`/`end` are **UTC Unix seconds**, integer, nonnegative, at most year 9999.
`start <= end` when both are supplied. Spot HTTP timestamps and private history
timestamps convert to milliseconds. Futures public candle times stay seconds.
Absent times remain absent so latest candles use exchange time, not local time.

Financial decimals accept positive plain decimal strings or JSON numbers, at
most 64 characters. Prefer strings for exact precision; the connector cannot
recover precision already lost by a caller's floating point representation.
NaN, infinities, exponents, zero, negatives, whitespace and missing required
amounts are rejected. Futures JSON numbers additionally reject leading zeros.
Signed monetary values are never round-tripped through `f64`. Normalized public
data intentionally uses the SDK's finite `f64` fields.

## Public operations (`Access::Public`)

| Operation | Additional parameters | Result |
| --- | --- | --- |
| `health` | none | `{ok: true, market}` on a successful ping |
| `markets` | none | SDK `Market[]`: `{symbol, base, quote, market}` |
| `ticker` | `symbol` | SDK `Ticker`: `{symbol, last, bid, ask, volume}`; optional numeric fields are null when absent |
| `candles` | `symbol`, `interval`; optional `start`, `end`, `limit` | SDK `Candle[]`, ascending time, duplicate times replaced by the last provider row |
| `order_book` | `symbol`; optional `limit` | `{symbol, bids: [[price, quantity]], asks: [[price, quantity]]}`; provider level order retained |
| `trades` | `symbol`; optional `limit` | `[{time, price, quantity, side: "buy" | "sell"}]`; aggressor side, time in seconds |
| `funding` | futures only, `symbol` | `{symbol, rate, next_time, interval_hours}`; negative rates allowed, next_time in seconds |

Futures volume/quantity uses **contracts**, not automatically converted base
currency. Spot volume/quantity uses the provider's base-asset units.
Markets include provider-listed contracts/pairs; listing is not a promise that
your account has permission to trade them. Inspect exchange trading rules before
trading; the exchange remains the authority for tick sizes, lot sizes, notional
limits and symbol/account-specific leverage limits.

Intervals:

| SDK input | Spot REST wire value | Futures REST wire value |
| --- | --- | --- |
| `1m` | `1m` | `Min1` |
| `5m` | `5m` | `Min5` |
| `15m` | `15m` | `Min15` |
| `30m` | `30m` | `Min30` |
| `1h` | `60m` | `Min60` |
| `4h` | `4h` | `Hour4` |
| `1d` | `1d` | `Day1` |
| `1w` | **unsupported_capability** | `Week1` |
| `1M` | `1M` | `Month1` |

Although spot docs advertise `1W`, both `1W` and `1w` returned HTTP 400 / code
`-1121` in public checks. We reject spot weekly candles explicitly. `1h` also
returned invalid interval; the documented `60m` works. No artificial resampling.

Futures candle responses are columnar `time/open/high/low/close/vol`; spot v3
rows are `[openTimeMs, open, high, low, close, volume, ...]`. The connector checks
matching column lengths, finite nonnegative values, timestamps and OHLC
consistency. It does not substitute zero for missing/malformed data. Futures
public candles do not accept an exchange row limit; the connector returns the
latest requested count from the returned bounded provider window. At most
8 MiB is read; it never paginates automatically to fill a sparse window.

### Candle history controls

Both ready candle sources keep their existing IDs (`spot_candles` and
`futures_candles`) and explicitly map `interval`, `end`, and `limit` from the
frontend port. Only frontend-declared controls are bound, so interval-only
frontends remain compatible. There is no fixed source `limit`: omitting it uses
the backend's live default of 200; requests may choose 1..1000.

For history, request `{interval: "1h", end: oldestTime - 1, limit: 200}` with
UTC Unix seconds. `end` bounds candle **open** time inclusively; subtracting one
second excludes the previous oldest candle without relying on local wall time.
Spot converts times to milliseconds. The spot provider ignores `endTime` alone,
so for end-only candle requests the connector also derives `startTime` from the
requested count and interval, aligned to UTC candle opens. Monthly windows use
calendar months (including leap years), not a fixed number of seconds. Explicit
`start` remains unchanged. Futures accepts `end` directly in seconds and needs
no derived `start`; its provider window is trimmed locally to the requested count.
Neither market adds time bounds to a latest/live request.

The opt-in public-only regression checks three strictly nonoverlapping pages
for both markets across every shared interval, with 200-bar hourly pages:

```sh
cargo test --manifest-path crates/ourbit/Cargo.toml live_public_historical_candles -- --ignored --nocapture
```

It never reads credentials or uses private endpoints. Network/WAF availability
and provider retention still apply; sparse or exhausted windows may be shorter.

## Private reads (`Access::PrivateRead`)

Successful signed operations return the **provider payload unchanged** (including
the futures success envelope), preserving native balances, fees, IDs and decimals.
These are not SDK-normalized account/order schemas.

| Operation | Spot additional params | Futures additional params |
| --- | --- | --- |
| `balances` | none | none |
| `positions` | **unsupported_capability** | optional `symbol` |
| `open_orders` | `symbol` | `symbol`; optional `page`, `limit` |
| `order_history` | `symbol`; optional `start`, `end`, `limit` | `symbol`; optional `start`, `end`, `page`, `limit` |
| `user_trades` | `symbol`; optional `order_id`, `start`, `end`, `limit` | `symbol`; optional `start`, `end`, `page`, `limit` |

Explicit spot order-history ranges cannot exceed seven days. Futures history
ranges are conservatively bounded to 90 days. Actual retention, available
periods and page bounds are exchange-dependent. Spot `open_orders` deliberately
requires a symbol, matching the documented API; no account-wide query fallback.

## Trading (`Access::Trade`)

Trading support is an implementation of the signed REST contract, **not a claim
that account permissions or order execution have been live verified**.

### Spot

- `place_order`: `symbol`, explicit uppercase `side: "BUY" | "SELL"`, explicit
  `type: "LIMIT" | "MARKET" | "LIMIT_MAKER" | "IMMEDIATE_OR_CANCEL" | "FILL_OR_KILL"`.
  Non-market types require `quantity` and `price`, reject `quote_quantity`.
  `MARKET BUY` requires only `quote_quantity`; `MARKET SELL` only `quantity`.
  Market orders reject `price`. Optional `client_order_id` (max 32 characters).
  No inferred side, type, time-in-force, or amount.
- `cancel_order`: `symbol` and **exactly one** of `order_id` (max 64) or
  `client_order_id` (max 32).
- `cancel_all`: required single `symbol`, never the entire account.

### Futures (native enums, not spot order semantics)

- `place_order`: required `symbol`, `side` (1 open long, 2 close short, 3 open
  short, 4 close long), `type` (1 limit, 2 post-only, 3 IOC, 4 FOK, 5 market,
  6 market-to-limit), `open_type` (1 isolated, 2 cross), `leverage` (1..500),
  and `quantity` (contract volume, mapped to `vol`).
  Non-market orders require `price`. Type 5 rejects caller price and sends the
  native required zero price. Optional `position_id` (positive i64),
  `position_mode` (1 hedge, 2 one-way), `client_order_id` (max 32, `externalOid`).
  Side, order type, margin mode and leverage are **always explicit**.
- `cancel_order`: `order_id`, a positive u64 **string**; sends a one-element
  native ID array. Does not accept a symbol or client ID.
- `cancel_all`: required single `symbol`. No account-wide cancellation.

Identifiers accept ASCII letters, digits, underscore and hyphen only. Stop,
conditional, batch, transfer, withdrawal, margin-mode-change and leverage-change
operations are not exposed.

## Endpoint and signature references

Authoritative spot v3 docs:
<https://ourbitdevelop.github.io/apidocs/spot_v3_en/>.
The old `/open/api/v2` spot API is deliberately not used.

| Operation | Spot path | Futures path (under `/api/v1`) |
| --- | --- | --- |
| health | `/api/v3/ping` | `/contract/ping` |
| markets | `/api/v3/exchangeInfo` | `/contract/detail` |
| ticker | `/api/v3/ticker/24hr` | `/contract/ticker` |
| candles | `/api/v3/klines` | `/contract/kline/{symbol}` |
| order_book | `/api/v3/depth` | `/contract/depth/{symbol}` |
| trades | `/api/v3/trades` | `/contract/deals/{symbol}` |
| funding | — | `/contract/funding_rate/{symbol}` |
| balances | `/api/v3/account` | `/private/account/assets` |
| positions | — | `/private/position/open_positions` |
| open_orders | `/api/v3/openOrders` | `/private/order/list/open_orders/{symbol}` |
| order_history | `/api/v3/allOrders` | `/private/order/list/history_orders` |
| user_trades | `/api/v3/myTrades` | `/private/order/list/order_deals` |
| place_order | POST `/api/v3/order` | POST `/private/order/submit` |
| cancel_order | DELETE `/api/v3/order` | POST `/private/order/cancel` |
| cancel_all | DELETE `/api/v3/openOrders` | POST `/private/order/cancel_all` |

Read endpoints use GET. Spot signed requests use `X-OURBIT-APIKEY`, `timestamp`
in milliseconds, `recvWindow=5000`, and a lowercase hex HMAC-SHA256 over the
**exact encoded query** excluding `signature`, keyed with the secret. All spot
fields, including POST/DELETE fields, are placed in the query, which v3 permits.

Futures signing uses `ApiKey`, `Request-Time` (milliseconds), `Recv-Window: 5`
and `Signature`: lowercase HMAC-SHA256(secret, key + timestamp + payload).
GET payloads are lexicographically sorted encoded query strings; POST payloads
are the exact serialized JSON body sent on the wire. Clocks must be synchronized
with exchange time. The connector does not silently widen time windows or retry
signature failures or adjust the machine clock.

**Futures verification caveat:** the official `contract_en` documentation link
was unavailable (404) during implementation. Public endpoints above were
checked against the real Ourbit origin. The private compatibility mapping and
native enums follow the Ourbit connector API shape; no private endpoint,
permission, signature acceptance, retention policy or mutation has been
verified with real credentials. Offline fixtures prove request construction,
not exchange acceptance. Review the current exchange contract and validate
account read permissions before enabling trading.

## Validation

```sh
cargo test --manifest-path crates/ourbit/Cargo.toml --all-targets
cargo clippy --manifest-path crates/ourbit/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path crates/ourbit/Cargo.toml -- --check
# Explicit opt-in, public GETs only; never reads credentials:
cargo test --manifest-path crates/ourbit/Cargo.toml live_public -- --ignored --nocapture
```

The offline suite uses actual loopback TCP HTTP requests, deterministic
independent HMAC vectors, exact signed-query/body assertions, decimal precision,
timestamp conversion, candle sorting/deduplication, malformed payloads, HTTP-200
error envelopes, redaction, access classifications, retries, cooldown, redirect
refusal, and size bounds. No test submits real orders or reads a real account.

The opt-in Rust live smoke was run successfully during implementation for both
markets: health, markets, ticker, candles, depth, trades, and futures funding.
The smoke also exercises every supported candle interval (8 spot, 9 futures).
Additional public GET probes verified the spot weekly rejection. Python urllib probes without
the connector user agent hit WAF 403; those are not treated as successful checks.
