//! Explicit public-network smoke for an installed executable candle provider.
//! Run after staging: cargo run --example package_smoke -- dist/extensions/ourbit
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tradeslope_package_sdk::{PackageKind, install};
use tradeslope_provider_process::{ProcessProvider, ProcessProviderConfig};
use tradeslope_sdk::{
    BackendHost, BoundInputs, Field, InputPort, OperationKind, Permissions, Schema, candle_schema,
    runtime::{BackendRuntime, Database},
    source_binding,
};

fn wait(request: tradeslope_sdk::BoundRequest) -> tradeslope_sdk::BackendResult<serde_json::Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match request.try_recv() {
            Ok(result) => return result,
            Err(std::sync::mpsc::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                return Err(tradeslope_sdk::BackendError::new(
                    "timeout",
                    "Package smoke timed out",
                ));
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("Pass a trusted provider package directory")?,
    );
    let temporary = tempfile::tempdir()?;
    let package = install(&source, temporary.path().join("extensions"))?;
    let PackageKind::Provider {
        command,
        args,
        backend,
        sources,
        settings,
    } = &package.manifest.kind
    else {
        return Err("Expected an executable provider package".into());
    };
    let source = sources
        .iter()
        .find(|source| {
            ["interval", "end", "limit"]
                .iter()
                .all(|control| source.controls.contains_key(*control))
        })
        .ok_or("Package has no historical candle source")?;
    let db_path = temporary.path().join("runtime.sqlite3");
    let provider = ProcessProvider::new(ProcessProviderConfig {
        package_root: package.root.clone(),
        command: command.into(),
        args: args.clone(),
        manifest: backend.clone(),
        sources: sources.clone(),
        settings: settings.clone(),
        db_path: db_path.clone(),
    })?
    .with_background_sync(true);
    let mut runtime = BackendRuntime::new(Database::open(db_path)?, vec![Arc::new(provider)])?;
    assert!(
        !runtime.states()[0].enabled,
        "Inspection/installation must not grant execution"
    );
    runtime.set_enabled(&backend.id, true)?;
    let host = BackendHost::new(runtime.backends())?;
    let client = host.client(Permissions::default());
    runtime.set_invalidator(Some(client.invalidator()))?;
    runtime.set_resetter(Some(client.resetter()))?;
    let port = InputPort {
        id: "bars".into(),
        title: "Historical candles".into(),
        kind: OperationKind::Query,
        parameters: Schema::object(BTreeMap::from([
            (
                "interval".into(),
                Field {
                    schema: Schema::Enum {
                        values: vec!["1h".into()],
                    },
                    required: true,
                    description: String::new(),
                    default: None,
                },
            ),
            (
                "end".into(),
                Field {
                    schema: Schema::Integer,
                    required: false,
                    description: String::new(),
                    default: None,
                },
            ),
            (
                "limit".into(),
                Field {
                    schema: Schema::Integer,
                    required: false,
                    description: String::new(),
                    default: None,
                },
            ),
        ])),
        output: candle_schema(),
        required: true,
    };
    let binding = source_binding(&client, &port, &backend.id, &source.id, json!({}))?;
    let inputs = BoundInputs::new(
        client,
        vec![port],
        BTreeMap::from([("bars".into(), binding)]),
    )?;
    let latest = wait(inputs.submit("bars", json!({"interval":"1h","limit":3}))?)?;
    let latest = latest.as_array().ok_or("Expected candle array")?;
    let first = latest
        .first()
        .and_then(|bar| bar["time"].as_i64())
        .ok_or("No latest candle")?;
    let older = wait(inputs.submit("bars", json!({"interval":"1h","end":first-1,"limit":3}))?)?;
    let older = older.as_array().ok_or("Expected historical candle array")?;
    assert!(!older.is_empty());
    assert!(
        older
            .iter()
            .all(|bar| bar["time"].as_i64().is_some_and(|time| time < first))
    );
    runtime.set_enabled(&backend.id, false)?;
    println!(
        "PASS: installed disabled, explicit enable, subprocess public candles, strictly older page, clean disable"
    );
    Ok(())
}
