# TradeSlope package SDK

UI-independent inventory, local-directory installation, and enablement. This crate
depends only on `tradeslope-sdk`, serde and serde_json; no GPUI, process adapter,
SQL backend implementation, shell, or provider-specific code.

## Host API

- `inspect(root: impl AsRef<Path>) -> Result<Package>`
- `discover(roots: &[PathBuf]) -> Vec<Result<Package>>`: roots contain immediate
  package directories. Missing roots are empty; errors remain visible. Duplicate
  IDs across all roots reject **all** occurrences. Hidden staging dirs are ignored.
- `install(source_dir, managed_root) -> Result<Package>`: validated local copy
  into `managed_root/<id>`, never replacing an existing package.
- `Package { root: PathBuf, manifest: PackageManifest }`: root is canonical.
- `PackageManifest { format_version, id, title, version, kind: PackageKind }`.
- `PackageKind::Provider { command, args, backend, sources, settings }` supplies
  static metadata for the host to construct its own process adapter. Resolve
  `command` with `package.resolve_file(command)`; spawn directly, never via shell
  or PATH lookup, and only after explicit trusted user enablement.
- `PackageKind::JavaScript { entry, card }`: load at startup using
  `package.read_resource(entry)` (bounded UTF-8); the host owns MIME handling.
  Binary resources can use `resolve_file` and a separately bounded binary reader.
- `Registry::load(&Database)`, `save(&Database)`, `set_enabled(id, bool)` and
  `is_enabled(&Package, PackageOrigin)`. Storage uses SDK namespace
  `app.extensions`, key `enabled`, a JSON ID-to-boolean map.
- `InventoryEntry::new(&Package, origin, &Registry)` returns ID, title, kind, path,
  enabled and restart_required without executing anything. `restart_required`
  describes the restart-only lifecycle, not comparison with a running inventory.

Only bundled JavaScript packages default enabled. Managed packages and all
providers default disabled. A stored `true` must represent explicit user trust,
never a manifest-provided preference. The host owns origin classification.
In the application, this registry controls frontend packages only. Providers are
registered metadata-only on restart and their actual enablement comes exclusively
from SDK `BackendRuntime::set_enabled` / runtime states. The host must replace the
provider inventory's preference snapshot with the runtime's enabled state rather
than maintain a second provider flag here.

## extension.json

```json
{
  "format_version": 1,
  "id": "example.provider",
  "title": "Example provider",
  "version": "1",
  "kind": "provider",
  "command": "bin/provider",
  "args": [],
  "backend": {
    "id": "example.provider", "title": "Example provider",
    "version": "1", "operations": []
  },
  "sources": [],
  "settings": {"schema": {"type": "object", "fields": {}}, "defaults": {}}
}
```

For JavaScript, replace command/args/backend/sources/settings with
`"kind": "javascript", "entry": "index.html", "card": <SDK CardManifest>`.
The card renderer is the SDK spelling `"java_script"`. Card/backend ID must match
the package ID. Legacy `schema_version: 2` manifests with only `entry` and `card`
are normalized to format 1 using the card ID/title and version `"1"`, including
the repository's quote example.

`native_source` returns `rebuild_required`: native source crates may continue to
use the native SDK when compiled into the application, but binary GPUI ABI
hot-loading is explicitly unsupported. Only providers and JavaScript hot-load.

## Bounds and trust boundary

IDs are 1–100 lowercase ASCII letters/digits plus `.`, `_`, `-`, start with a
letter/digit, and cannot contain `..`, end with `.`, or be Windows device names.
Entries and commands must be contained regular files without symlink components,
traversal, absolute paths, drive prefixes or backslashes. Commands must have
executable bits on Unix. Args must be JSON strings with no NUL/line separators.
Arguments are **public static metadata, never secrets**: credential values must
be supplied through the application's environment-reference mechanism. The SDK
cannot infer whether an arbitrary string is a secret and does not inspect content.

Provider registration uses a metadata-only SDK `BackendHost` dummy whose execute
always fails; inspection never launches executables or calls lifecycle hooks.
Source validation comes from SDK registration. Settings require an object schema
and valid defaults. Schema field defaults are checked recursively.

Installation stages within the managed root and renames only after copied-package
validation. A per-ID create-new lock serializes SDK installs, with cleanup on
failure. Hosts must not concurrently mutate package filesystem trees externally;
this is not a defense against another local process changing paths mid-operation.
Stale locks after process termination require explicit cleanup by the host/user.
Successful rename is atomic visibility, not a promise of crash-durable fsync.

Installation rejects symlinks/special files, `.env`/`.env.*`, common SSH private
key filenames and `.pem`/`.key`/`.p12`/`.pfx` files; it skips `.git`, `target`,
`node_modules`. The conservative filename policy also excludes public PEM files.
No arbitrary file-content inspection occurs. Limits: 4096 filesystem entries
(directories count too), 256 MiB total, 128 MiB per file, depth 64. Frontend text
resources and manifest JSON are limited to 8 MiB each.
