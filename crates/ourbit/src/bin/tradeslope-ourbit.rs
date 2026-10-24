//! Installable Ourbit provider: metadata on explicit request, otherwise SDK stdio.
use serde_json::json;
use std::{io::Write, process::ExitCode, sync::Arc};
use tradeslope_ourbit::{FUTURES_ENDPOINT, Ourbit, SPOT_ENDPOINT};
use tradeslope_sdk::Backend;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tradeslope-ourbit: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), &'static str> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        println!(
            "Usage: tradeslope-ourbit [--manifest | --help]\n\
             No arguments: serve the TradeSlope provider protocol on stdin/stdout.\n\
             --manifest: print package metadata as JSON without network access."
        );
        return Ok(());
    }
    let manifest = args.len() == 1 && args[0] == "--manifest";
    if !args.is_empty() && !manifest {
        return Err("unsupported arguments; use --help");
    }

    // Runtime account references arrive through SDK services and are resolved
    // inside the provider. Never use the standalone environment constructor here.
    let ourbit = Ourbit::with_endpoints(SPOT_ENDPOINT, FUTURES_ENDPOINT, None, None)
        .map_err(|_| "could not initialize provider")?;
    if manifest {
        let package = json!({
            "format_version": 1,
            "id": "ourbit",
            "title": "Ourbit",
            "version": env!("CARGO_PKG_VERSION"),
            "kind": "provider",
            "command": if cfg!(windows) {
                "bin/tradeslope-ourbit.exe"
            } else {
                "bin/tradeslope-ourbit"
            },
            "args": [],
            "backend": ourbit.manifest(),
            "sources": ourbit.sources(),
            "settings": ourbit.settings(),
        });
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &package)
            .map_err(|_| "could not write manifest")?;
        writeln!(stdout).map_err(|_| "could not write manifest")?;
        return Ok(());
    }

    // Keep non-protocol diagnostics off stdout and avoid exposing backend errors
    // that could contain request details or process credentials.
    tradeslope_provider_process::serve(Arc::new(ourbit)).map_err(|_| "provider protocol failed")
}
