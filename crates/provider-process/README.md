# Trusted process providers

`ProcessProvider` implements the SDK `Backend` contract using an explicitly
installed executable. This is **not a sandbox**, a native ABI loader, or a
credential vault. A trusted provider can access the filesystem and inherited
environment with the user's privileges. Networking/TLS belongs in the provider,
not this adapter.

## Host API

Construct `ProcessProvider::new(ProcessProviderConfig { package_root, manifest,
sources, settings, command, args, db_path })`. Metadata fields are owned SDK
types; paths are `PathBuf` and args are `Vec<String>`. The command must be
relative; at startup it is canonicalized and required to remain under the
canonical package root. It is launched directly, without a shell, with that root
as cwd and inherited stderr. Database paths are made absolute in the host.

`manifest()`, `sources()`, and `settings()` are static package metadata and never
launch a process. Fresh external providers default disabled; only explicit
application enablement should launch them. The ordinary SDK runtime owns
persisted enablement/settings/account selection.

Startup requests metadata from one persistent process, compares its actual
manifest, sources, and settings metadata with the package, and rejects any
difference **before sending the database path/context or starting the backend**.
Only after this comparison does initialize open the database and start services.
Default deadlines are 5 seconds for the complete metadata/initialization handshake and
30 seconds per context/execute request. `with_timeouts(handshake, execute)`
overrides these for testing or installation policy. No request is retried,
including mutations with ambiguous completion after a timeout.

Before **every** execute, the adapter synchronizes settings, account profiles
(environment-variable references only), and active account ID. Optional
`with_background_sync(true)` adds a 250ms idle sync for providers doing work
without consumer requests. It is disabled by default. The application must not
put resolved credential values into settings or profiles.

Cancellation is polled while waiting. A deadline, cancellation, framing failure,
or mismatched response terminates the connection; further execution fails until
an explicit stop/start. Stop can kill/reap without taking the blocked RPC/I/O
lock. Drop also stops the process and joins adapter workers. Unix pipes use
nonblocking I/O with independent cancellation checks; even a detached descendant
retaining stdin/stdout cannot prevent I/O-worker shutdown. Unix children also run
in their own process group for best-effort descendant cleanup.

**Process execution is currently supported only on Unix.** On other platforms,
start fails closed with `process_io_unsupported` before spawning a child.
Implementing platform-specific cancellable pipes is required before enabling
those platforms; blocking pipe threads with indefinite joins are not a fallback.

## Provider executable

Call `serve(Arc<dyn Backend>)`. The server opens the supplied application SQLite
database and creates a trusted SDK `ProviderSession`, namespaced by the actual
backend ID. This starts/stops the backend exactly once, without creating a
second `BackendRuntime` or changing persisted enabled state. The SDK host
dispatcher provides strict parameter and output validation. Permission grants
remain the parent application's responsibility; the runner does not invent a
second permission policy.

Stdout is exclusively framed protocol output: **never log to stdout**. Use
stderr. All writes, including service notifications, share one stdout mutex.
Malformed input, provider errors, and transport failures are reported with
redacted diagnostics, never raw frames or child error text.

## Wire format (version 1)

Every frame is one UTF-8 JSON object followed by LF. Request IDs are positive,
strictly increasing `u64`s; responses must match the outstanding ID and version.
Only one request is outstanding. Request frames are bounded to 1 MiB + 64 KiB;
execute `params` alone may encode to at most 1 MiB. Response/notification frames
including LF are bounded to 16 MiB. Readers enforce limits before unbounded
allocation. Protocol structures live in `protocol` for non-Rust implementations.

Request methods are `metadata`, `initialize`, `context`, `execute`, and `shutdown`.
Metadata is requested first and contains no database path or configuration:

```json
{"version":1,"id":1,"method":"metadata"}
{"version":1,"id":2,"method":"initialize","params":{"db_path":"/absolute/app.db","context":{"settings":{},"accounts":[],"selected":null}}}
{"version":1,"id":3,"method":"context","params":{"settings":{},"accounts":[],"selected":null}}
{"version":1,"id":4,"method":"execute","params":{"operation":"markets","params":{}}}
{"version":1,"id":5,"method":"shutdown"}
```

Responses use serde's externally tagged `Result` encoding. Metadata returns
`{"manifest":...,"sources":[...],"settings":...}`; initialize/context/shutdown return null:

```json
{"type":"response","version":1,"id":2,"result":{"Ok":null}}
{"type":"response","version":1,"id":3,"result":{"Err":{"code":"provider_error","message":"Provider process request failed"}}}
{"type":"changed","version":1}
```

A `changed` notification only calls host `BackendServices::notify_changed()`.
It invalidates consumers; it never publishes consumer data or grants access.
EOF/shutdown cancels the provider session and calls backend stop. A host needing
to interrupt blocked execution kills the process rather than attempting a
second stdin request or retry.

## Verification

`cargo test -p tradeslope-provider-process --features test-fixture` builds a
deterministic fixture executable and tests real spawned processes. It performs
no live network/private calls. The test-only binary is feature-gated and is not
part of normal provider builds.
