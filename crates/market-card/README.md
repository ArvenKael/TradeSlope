# tradeslope-market-card

A native GPUI `Render` entity implementing `NativeCardFactory`, with no webview
and no concrete backend dependency. Its only data access is the mapped
`CardContext.inputs` supplied by the host. Native cards are compiled into the
host with compatible SDK and GPUI dependencies, not loaded as binary plugins.

## Integration

Register `Box::new(tradeslope_market_card::MarketCardFactory)` in your host's
native factory registry under `factory.manifest().id` (`market-overview`). Call
`factory.create(context, window, cx)`, retain the returned instance, and embed
`instance.view()` in your GPUI layout. Call `instance.set_context(...)` whenever
the host changes the input mappings or client. See `../native-sdk/README.md` for a
complete third-party factory and host-owned registration example.

These crates can be built independently with
`cargo check --manifest-path crates/market-card/Cargo.toml` once the adjacent
`../extension-sdk` crate is present.

## Behavior and input contract

- Declares a required Query input, `quote` (title `Quote`), and an optional
  `comparison` Query (title `Quote comparison`). Both use empty object
  parameters and the standard `ticker_schema()` object output.
  The user chooses and maps a backend API in Connections; the card never names
  a provider or API operation.
- Map the optional comparison to a different backend to display two independent
  ticker sources in the built-in native card. Each owns its own subscription,
  snapshot, and error state; comparison failure/reset cannot overwrite the
  primary quote. One UI consumption task serves both without extra fetch timers.
  A full context replacement currently resets both subscriptions.
- A disconnected input displays “Map the Quote input using Connections”.
  A mapped input displays “Quote input connected”. No default provider,
  synthetic quote, or invented zero is used.
- Subscribes with `context.inputs.subscribe("quote", json!({}))` and decodes the mapped,
  validated result as `tradeslope_sdk::Ticker`. Displays last price, bid, ask,
  volume, last changed time (UTC), and connection errors.
  `last` is required; optional numbers render as `—`.
- One entity-owned, weak-handle GPUI task only consumes latest updates and
  notifies when they change, yielding every 100 ms. It never schedules backend
  fetches. The shared SDK hub owns worker execution, single-flight requests,
  five-second fallback polling (host configurable), and change-only delivery.
  The native Refresh/Retry button calls `subscription.refresh()`.
- Matching mapped backend/operation/arguments share one query across cards.
  Delivery retains one latest value rather than queuing every tick. An unchanged
  quote is not a new data change and does not become stale solely because its
  price has not changed. Errors retain the previous snapshot marked STALE.
- Every context replacement, even with identical mappings, clears cached
  data/errors and drops the old subscription before creating a new one.
- Dropping all entity/view handles cancels the owned task. The async loop holds
  no strong entity reference. Retaining `AnyView` intentionally retains its
  entity; drop it too when removing a card.

Dropping the last subscription cancels its worker and fallback polling. A request
already submitted to a backend may continue server-side: this is not a promise
of transport-level cancellation, and backends must set I/O deadlines.

This subscription API is not itself a streaming transport. Ourbit REST queries
use fallback polling, not true push. Provider notifications can use
`invalidate(backend)` to force a refresh without waiting for the polling interval.
There is no consumer snapshot-publication API: values always come from backend
execution and use the normal schema/projection validation. Account/settings
changes and backend disable must use `reset(backend)` (or the weak `resetter()`
runtime hook) to clear cached data immediately and fence pre-reset responses.

## Validation

```sh
cargo test --manifest-path crates/market-card/Cargo.toml --lib
cargo check --manifest-path crates/native-sdk/Cargo.toml
```

Pure-state tests cover changed snapshots, errors with retained data, recovery,
context state reset, invalid ticker responses, and missing/non-finite numeric
formatting. SDK tests cover sharing, cancellation, query guards, bounded latest
delivery and invalidation. A host window is needed for manual rendering and
lifecycle interaction checks.
