//! Explicitly invoked SDK client. No credentials in arguments, files, or logs.
use serde_json::Value;
use std::{env, process::ExitCode, sync::Arc};
use tradeslope_ourbit::Ourbit;
use tradeslope_sdk::{BackendError, BackendHost, BackendResult, Permissions};

const USAGE: &str = r#"Ourbit SDK example (default: public only)

Usage:
  ourbit [--private-read] [--allow-trade --confirm-real-order] OPERATION 'PARAMS_JSON'

Examples:
  cargo run --manifest-path crates/ourbit/Cargo.toml --example ourbit -- ticker '{"market":"spot","symbol":"BTC_USDT"}'
  cargo run --manifest-path crates/ourbit/Cargo.toml --example ourbit -- --private-read balances '{"market":"spot"}'

Set credentials only through OURBIT_API_KEY / OURBIT_API_SECRET and optional
OURBIT_FUTURES_API_KEY / OURBIT_FUTURES_API_SECRET, not command-line arguments.
--allow-trade requires --confirm-real-order before any mutation is permitted.
Unverified private API acceptance and financial risk remain the caller's responsibility.
No command runs when --help is supplied. Successful payloads are printed to stdout;
account payloads may contain sensitive financial information.
"#;

struct Command {
    operation: String,
    params: Value,
    permissions: Permissions,
}

fn parse(args: Vec<String>) -> BackendResult<Option<Command>> {
    if args.is_empty() || args.iter().any(|s| s == "--help" || s == "-h") {
        return Ok(None);
    }
    let mut private_read = false;
    let mut trade = false;
    let mut confirm = false;
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--private-read" if !private_read => private_read = true,
            "--allow-trade" if !trade => trade = true,
            "--confirm-real-order" if !confirm => confirm = true,
            _ if arg.starts_with("--") => {
                return Err(BackendError::new("usage", "Unknown or duplicate option"));
            }
            _ => positional.push(arg),
        }
    }
    if positional.len() != 2 {
        return Err(BackendError::new(
            "usage",
            "Expected an operation and a JSON object; see --help",
        ));
    }
    if trade != confirm {
        return Err(BackendError::new(
            "usage",
            "Trading requires both --allow-trade and --confirm-real-order",
        ));
    }
    let params: Value = serde_json::from_str(&positional[1])
        .map_err(|_| BackendError::new("usage", "Invalid params JSON"))?;
    if !params.is_object() {
        return Err(BackendError::new("usage", "Params must be a JSON object"));
    }
    Ok(Some(Command {
        operation: positional.remove(0),
        params,
        permissions: Permissions {
            private_read,
            trade,
        },
    }))
}

fn run() -> BackendResult<()> {
    let Some(command) = parse(env::args().skip(1).collect())? else {
        print!("{USAGE}");
        return Ok(());
    };
    let host = BackendHost::new(vec![Arc::new(Ourbit::from_env()?)])?;
    let client = host.client(command.permissions);
    let response = client.call("ourbit", &command.operation, command.params)?;
    let output = serde_json::to_string_pretty(&response)
        .map_err(|_| BackendError::new("output", "Cannot serialize response"))?;
    println!("{output}");
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).into()).collect()
    }

    #[test]
    fn public_by_default_and_mutations_require_both_flags() {
        let command = parse(arguments(&[
            "ticker",
            r#"{"market":"spot","symbol":"BTCUSDT"}"#,
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(command.permissions, Permissions::default());
        for args in [
            vec!["--allow-trade", "cancel_all", "{}"],
            vec!["--confirm-real-order", "cancel_all", "{}"],
            vec!["--unknown", "ticker", "{}"],
        ] {
            assert!(parse(arguments(&args)).is_err());
        }
        let command = parse(arguments(&[
            "--private-read",
            "--allow-trade",
            "--confirm-real-order",
            "cancel_all",
            r#"{"market":"spot","symbol":"BTCUSDT"}"#,
        ]))
        .unwrap()
        .unwrap();
        assert!(command.permissions.private_read && command.permissions.trade);
        assert!(parse(arguments(&["--help"])).unwrap().is_none());
    }
}
