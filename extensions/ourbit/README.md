# Ourbit provider package

Ourbit runs as a separate executable using the TradeSlope process SDK over
stdin/stdout. It is not linked into the host application. The native GPUI market
card example remains a separate Rust crate; this package does not build it.

## Prepare local packages

From the repository root:

```sh
sh scripts/build-extensions.sh
# Faster development build:
sh scripts/build-extensions.sh --debug
```

The default is a release build for the current host. Rust/Cargo is required; npm
is not. The script builds the actual provider executable, generates its manifest
without network requests, and copies an explicit list of public OpenAlgo assets
(including their manifest and upstream licenses):

```text
dist/extensions/
  ourbit/
    extension.json
    bin/tradeslope-ourbit[.exe]
  openalgo/
    extension.json
    index.html
    card.js
    card.css
    README.md
    vendor.sha256
    vendor/...
```

`dist/extensions` is the prepared discovery root. `extensions/ourbit` is source
documentation, not an executable package. The frontend source package currently
lives in `extensions/openalgo`. Configure any host extension-path override to point
at the prepared root, not this documentation directory.

The script neither builds the host application nor installs or publishes
anything. Build output stays under the workspace's `target` and `dist`
directories. Do not commit generated binaries. No `.env`, account configuration,
or private files are copied.

## Executable contract

- No arguments: serve the SDK protocol on stdin/stdout.
- `--manifest`: print one version-1 JSON provider package manifest and exit.
  Metadata includes backend operations, data sources and settings; it performs no
  network requests and contains no process credentials.
- `--help`: print human-readable usage to stdout and exit.
- Any other arguments: exit with status 1; diagnostics use stderr.

The package command is relative to its own directory:
`bin/tradeslope-ourbit` (`bin/tradeslope-ourbit.exe` on Windows).

The executable constructs Ourbit with the official HTTPS endpoints and no
constructor credentials. Runtime account selection comes from SDK services;
environment references are resolved only inside the provider. The standalone
`Ourbit::from_env` library constructor is intentionally not used by this process.
Never put actual API keys or secrets into a package manifest.
