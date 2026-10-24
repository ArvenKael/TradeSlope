# tradeslope-sdk

UI-independent Rust backend and extension registration contracts. Runtime
dependencies are only `serde` (derive) and `serde_json`. All public types are
exported at the crate root.

## Native backend dispatch

Construct a `BackendHost` from `Vec<Arc<dyn Backend>>`, then obtain a
`BackendClient` using explicit `Permissions`. Default permissions allow only
public operations. Private-read and trade permissions are independent.

`BackendClient::submit` validates the cached registry, operation, permissions,
parameter schema, and JSON-encoded parameter size before nonblocking queue admission. Poll the
returned `PendingRequest::try_recv` from the UI. `call` is a blocking convenience
for non-UI callers, with a 30-second reply timeout.

Each host owns one dedicated worker with 64 waiting slots, shared by all its
clients. Backend execution is serialized. Parameters may encode to at most
1 MiB, including JSON punctuation and escaping. A dropped receiver does not
stop the worker. The last host/client dropping closes the queue; accepted work
drains without blocking the dropping thread. Timeouts do not cancel work:
backends must enforce their own I/O deadlines. A stalled backend stalls that
host's worker, but never causes `submit` to wait for execution or queue space.

SDK-generated error codes are `invalid_manifest`, `duplicate_backend`,
`duplicate_operation`, `unknown_backend`, `unknown_operation`, `invalid_params`,
`invalid_output`, `invalid_binding`, `unknown_input`, `unbound_input`, `duplicate_input`,
`permission_denied`, `params_too_large`, `queue_full`, `disconnected`, `timeout`,
`unsupported_version`, and `internal_error`. Backend-returned errors pass
through unchanged. Unwinding panics in backend execution or manifest discovery
become generic `internal_error` responses without panic details. The SDK does
not replace the process-wide panic hook; the application's hook can still log
panics. Abort-on-panic builds cannot recover from panics.

## JSON bridge and extension metadata

`BackendClient::submit_request` accepts protocol version 1 for trusted raw backend
callers. `BoundInputs::submit_request` accepts `InputRequest` version 2, containing
only `version`, `id`, `port`, and `params`; frontend callers never choose backend
or operation IDs. The bridge owner retains the
request ID and constructs `BackendResponse` with either a result or an error.
No transport, JavaScript engine, credentials, or UI toolkit is included.

`Candle.time` is an `i64` Unix timestamp in UTC seconds. `Ticker` uses `symbol`
and `last`, with optional `bid`, `ask`, and `volume`. Operation names and
parameter shapes belong to each backend; the SDK does not prescribe ticker
query parameters.

`ExtensionRegistry` validates globally unique extension IDs and globally
unique card IDs, plus unique input IDs within each card, rejecting
empty/whitespace-only IDs, titles, and version fields.
Versions are opaque nonempty strings, not parsed semantic versions.
Native factories belong in a separate UI-specific crate.

## Declarative source discovery

Backends explicitly implement `Backend::sources() -> Vec<DataSource>` for the
straightforward card-local picker. The default is empty (advanced operation
binding only); no operation names or parameter semantics are guessed.
`BackendClient::sources()` returns captured `SourceDescriptor` envelopes with
`backend`, `backend_title`, and `source`. Runtime wrappers forward discovery.

`DataSource` declares an ID/title, a public Query `operation`, fixed `params`,
an object `configuration` containing only user setup fields, and `controls`
mapping backend argument names to card-local frontend argument names.
An optional `SourceCatalog` declares a public Query with complete fixed params,
an array-of-objects output, required string `value_field`/`label_field`, and
the destination configuration `parameter`. The UI copies the selected row's
value verbatim into that parameter; normalization belongs to the provider.
Catalog requests are read-only, but are still submitted explicitly by the host.
Descriptors and catalogs are validated at backend registration (`invalid_source`).

`source_binding(&client, &port, backend_id, source_id, configuration)` validates
strict setup keys, applies configuration field defaults, checks typed output and
control compatibility, and returns an ordinary `PortBinding` without network
calls. It uses exact output names only, not heuristic renames. Dynamic controls
are removed from constants so descriptor defaults cannot override the frontend's
current interval. Unknown source IDs return `unknown_source`.
Source discovery does not change `Operation` or `InputPort` struct literals,
raw protocol v2 mappings, or existing/legacy bindings: advanced constants still
win over dynamic values there.

## Typed ports and user mappings

Every `Operation` advertises its `OperationKind`, `parameters`, and `output`.
`Operation::query(id, title, parameters, output)` creates a public query;
set `kind`/`access` explicitly for actions. Permission and action kind are
independent. The worker checks successful backend results against `output`,
returning `invalid_output` for invalid results.

`CardManifest.inputs` lists `InputPort`s with expected schemas but no provider
references. Each block instance owns `Bindings` mapping its port IDs to
`PortBinding { backend, operation, params, fields, parameter_fields }`:

- `params` contains constant backend arguments.
- `fields` maps frontend output names to backend output names, for objects or
  arrays of objects. Unmapped names use identity. Mapped objects are projected
  to declared target fields; primitive and `Any` results pass through unchanged.
- `parameter_fields` maps backend argument names to declared frontend argument
  names explicitly, even when the names match. There is no implicit parameter
  identity fallback. Only mapped backend arguments are projected, and constants
  override dynamic arguments. Unknown constants or mapping names are rejected.

`validate_binding` and `BoundInputs::new` check metadata only, never execute
operations, and reject insufficient permissions or mismatched action/query
kinds. Required backend arguments must be covered by constants or compatible,
required frontend arguments with explicit `parameter_fields` entries. Optional
backend arguments without constants or mappings are omitted. Required ports may be left unbound while a user
configures a block. `connected` reports binding presence.

`BoundInputs::submit(port, params)` validates frontend arguments, projects and
merges backend arguments, then submits nonblocking. Poll `BoundRequest::try_recv`
for the mapped, validated frontend result. Each request owns a snapshot of its
mapping; other block instances and later configuration cannot change it.

`Schema` is a serde internally tagged enum (`type`, snake_case), with local
primitive, enum, nullable, array, tuple, union, and object schemas. A source
integer satisfies a target number, but not vice versa. A nullable source cannot
satisfy a non-nullable target. `Any` accepts arbitrary values but cannot prove a
concrete output type. Union sources require every variant to fit; union targets
accept a matching variant. Tuples have exact length. No coercion, semantic
conversion, references, or network resolution occurs.

Output schema validation allows extra fields. Request validation rejects
undeclared object keys recursively, including inside arrays, tuples, unions,
and nullable values; explicit `Any` schemas remain open. Argument projection
reads only declared frontend fields and excludes undeclared backend keys for
typed object parameters. `Field.required` means present, including when
the value may be null; defaults and descriptions are metadata only.
`candle_schema`, `ticker_schema`, and `market_schema` describe serialized SDK
values. Nullable ticker fields remain required because serialization emits null.

`compatible_output` checks the actual mapping. `potentially_compatible` allows
field renames for candidate discovery, but does not validate a final binding.
`suggest_output_mapping` suggests only exact-name compatible fields, never
arbitrary numeric fields as OHLC values.

`compatible_parameters` checks strict input compatibility recursively: a mapped
nested object may not advertise keys absent from its backend argument schema,
even if those extra keys are optional. Parameter values are forwarded unchanged,
not projected internally. Output compatibility remains permissive of extras.
`schema::output_fields` (also exported at the root) finds record fields through
any number of nullable/array wrappers. Explicit output field mappings through
union or tuple wrappers are rejected; the SDK does not guess a variant to map.

For intentionally untyped backend parameters, `Schema::Any` permits arbitrary
constants and dynamic arguments (objects merge, constants win). Explicit
parameter-field mappings require declared object schemas. For non-object
parameter schemas, empty constant `{}` forwards frontend arguments; otherwise
the constant value supplies the entire backend argument.

## Checks

```sh
cargo test --manifest-path crates/extension-sdk/Cargo.toml
cargo fmt --manifest-path crates/extension-sdk/Cargo.toml --check
cargo clippy --manifest-path crates/extension-sdk/Cargo.toml --all-targets -- -D warnings
```
