use super::*;
use serde_json::{Value, json};
use tempfile::TempDir;

fn js() -> Value {
    json!({
        "format_version": 1, "id": "example.quote", "title": "Quote", "version": "1",
        "kind": "javascript", "entry": "index.html",
        "card": {"id":"example.quote", "title":"Quote", "renderer":"java_script", "inputs":[]}
    })
}
fn write_package(root: &Path, manifest: &Value) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("extension.json"),
        serde_json::to_vec(manifest).unwrap(),
    )
    .unwrap();
    fs::write(root.join("index.html"), "<html>quote</html>").unwrap();
}
fn provider(root: &Path) -> Value {
    fs::create_dir_all(root.join("bin")).unwrap();
    // Were inspection ever to spawn this, it would leave a marker.
    fs::write(root.join("bin/provider"), "#!/bin/sh\ntouch executed\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("bin/provider"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    json!({
        "format_version":1, "id":"example.provider", "title":"Provider", "version":"1",
        "kind":"provider", "command":"bin/provider", "args":[],
        "backend":{"id":"example.provider", "title":"Provider", "version":"1", "operations":[]},
        "sources":[], "settings":{"schema":{"type":"object","fields":{}},"defaults":{}}
    })
}
fn manifest_error(mut change: impl FnMut(&mut Value), code: &str) {
    let temp = TempDir::new().unwrap();
    let mut value = js();
    change(&mut value);
    write_package(temp.path(), &value);
    assert_eq!(inspect(temp.path()).unwrap_err().code, code);
}

#[test]
fn manifest_validation_and_resources() {
    manifest_error(|m| m["format_version"] = json!(2), "unsupported_version");
    for id in ["../bad", "Bad", "", "a/b", "a\\b", "con", "a..b"] {
        manifest_error(|m| m["id"] = json!(id), "invalid_id");
    }
    manifest_error(|m| m["id"] = json!("x".repeat(101)), "invalid_id");
    manifest_error(|m| m["card"]["id"] = json!("different"), "id_mismatch");
    manifest_error(
        |m| m["card"]["renderer"] = json!("native"),
        "invalid_renderer",
    );
    manifest_error(|m| m["command"] = json!("sh"), "invalid_manifest");
    manifest_error(|m| m["kind"] = json!("native_source"), "rebuild_required");
    for path in [
        "../index.html",
        "/index.html",
        "C:\\index.html",
        "a/../index.html",
    ] {
        manifest_error(|m| m["entry"] = json!(path), "unsafe_path");
    }
    let temp = TempDir::new().unwrap();
    write_package(temp.path(), &js());
    let package = inspect(temp.path()).unwrap();
    assert_eq!(
        package.read_resource("index.html").unwrap(),
        "<html>quote</html>"
    );
    assert!(package.read_resource("../extension.json").is_err());
    fs::write(temp.path().join("index.html"), [0xff]).unwrap();
    assert_eq!(inspect(temp.path()).unwrap_err().code, "invalid_utf8");
    fs::File::create(temp.path().join("index.html"))
        .unwrap()
        .set_len(MAX_FRONTEND_BYTES + 1)
        .unwrap();
    assert_eq!(inspect(temp.path()).unwrap_err().code, "size_limit");
}

#[test]
fn legacy_quote_example_loads() {
    // This test remains runnable in a standalone copy containing examples.
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/extensions/quote");
    let package = inspect(example).unwrap();
    assert_eq!(package.manifest.id, "example.quote");
    assert_eq!(package.manifest.version, "1");
    assert!(matches!(
        package.manifest.kind,
        PackageKind::JavaScript { .. }
    ));
    assert!(package.read_resource("index.html").unwrap().contains("<"));
}

#[test]
fn metadata_validation_never_launches_provider() {
    let temp = TempDir::new().unwrap();
    let mut value = provider(temp.path());
    write_package(temp.path(), &value);
    let package = inspect(temp.path()).unwrap();
    assert!(matches!(
        package.manifest.kind,
        PackageKind::Provider { .. }
    ));
    assert!(!temp.path().join("executed").exists());
    value["sources"] = json!([{
        "id":"bad", "title":"Bad", "operation":"missing", "params":{},
        "configuration":{"type":"object","fields":{}}, "controls":{}, "catalog":null
    }]);
    write_package(temp.path(), &value);
    assert_eq!(inspect(temp.path()).unwrap_err().code, "invalid_metadata");
    value["sources"] = json!([]);
    value["settings"]["defaults"] = json!("bad");
    write_package(temp.path(), &value);
    assert!(inspect(temp.path()).is_err());
    value["settings"]["defaults"] = json!({"undeclared":"no"});
    write_package(temp.path(), &value);
    assert!(inspect(temp.path()).is_err());
    value["settings"]["defaults"] = json!({});
    value["command"] = json!("provider-on-path");
    write_package(temp.path(), &value);
    assert!(inspect(temp.path()).is_err());
    value["command"] = json!("bin/provider");
    value["args"] = json!([123]);
    write_package(temp.path(), &value);
    assert!(inspect(temp.path()).is_err());
    assert!(!temp.path().join("executed").exists());
}

#[test]
fn duplicate_ids_reject_all_occurrences_across_roots() {
    let temp = TempDir::new().unwrap();
    let first = temp.path().join("bundled");
    let second = temp.path().join("managed");
    write_package(&first.join("one"), &js());
    write_package(&second.join("two"), &js());
    let results = discover(&[first, second, temp.path().join("absent")]);
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|result| result.as_ref().unwrap_err().code == "duplicate_id")
    );
}

#[test]
fn install_commits_and_never_overwrites() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let managed = temp.path().join("managed");
    write_package(&source, &js());
    fs::create_dir_all(source.join("node_modules")).unwrap();
    fs::write(source.join("node_modules/junk"), "not installed").unwrap();
    let package = install(&source, &managed).unwrap();
    assert_eq!(
        package.root,
        managed.canonicalize().unwrap().join("example.quote")
    );
    assert!(!package.root.join("node_modules").exists());
    assert_eq!(
        install(&source, &managed).unwrap_err().code,
        "already_installed"
    );
    assert_eq!(
        package.read_resource("index.html").unwrap(),
        "<html>quote</html>"
    );
    assert_eq!(fs::read_dir(&managed).unwrap().count(), 1);
    // An existing empty directory must not be overwritten either.
    fs::create_dir(managed.join("other")).unwrap();
    let mut value = js();
    value["id"] = json!("other");
    value["card"]["id"] = json!("other");
    write_package(&source, &value);
    assert_eq!(
        install(&source, &managed).unwrap_err().code,
        "already_installed"
    );
}

#[test]
fn failed_copy_and_oversize_leave_no_staging_or_destination() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let managed = temp.path().join("managed");
    write_package(&source, &js());
    fs::write(source.join(".env"), "do not copy").unwrap();
    assert_eq!(install(&source, &managed).unwrap_err().code, "private_file");
    assert_eq!(fs::read_dir(&managed).unwrap().count(), 0);
    fs::remove_file(source.join(".env")).unwrap();
    fs::File::create(source.join("large.bin"))
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();
    assert_eq!(install(&source, &managed).unwrap_err().code, "size_limit");
    assert_eq!(fs::read_dir(&managed).unwrap().count(), 0);
}

#[test]
fn failed_staged_validation_cleans_up_and_provider_executable_bits_survive() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let managed = temp.path().join("managed");
    let mut value = js();
    value["entry"] = json!("target/index.html");
    write_package(&source, &value);
    fs::create_dir(source.join("target")).unwrap();
    fs::write(source.join("target/index.html"), "<html></html>").unwrap();
    assert!(inspect(&source).is_ok());
    // The source entry is excluded from installation, so staged inspection fails.
    assert!(install(&source, &managed).is_err());
    assert_eq!(fs::read_dir(&managed).unwrap().count(), 0);
    let value = provider(&source);
    write_package(&source, &value);
    let installed = install(&source, &managed).unwrap();
    assert!(installed.resolve_file("bin/provider").unwrap().is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            installed
                .resolve_file("bin/provider")
                .unwrap()
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }
    assert!(!source.join("executed").exists());
    assert!(!installed.root.join("executed").exists());
}

#[test]
fn too_many_entries_and_recursive_destination_are_rejected() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    write_package(&source, &js());
    assert_eq!(
        install(&source, source.join("managed")).unwrap_err().code,
        "unsafe_path"
    );
    let managed = temp.path().join("managed");
    for i in 0..MAX_FILES {
        fs::create_dir(source.join(format!("empty-{i}"))).unwrap();
    }
    assert_eq!(install(&source, &managed).unwrap_err().code, "size_limit");
    assert_eq!(fs::read_dir(&managed).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn symlinks_are_rejected_for_entries_resources_and_copy() {
    use std::os::unix::fs::symlink;
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let managed = temp.path().join("managed");
    write_package(&source, &js());
    fs::write(temp.path().join("outside"), "outside").unwrap();
    symlink(temp.path().join("outside"), source.join("linked")).unwrap();
    let package = inspect(&source).unwrap();
    assert_eq!(
        package.read_resource("linked").unwrap_err().code,
        "unsafe_path"
    );
    assert_eq!(install(&source, &managed).unwrap_err().code, "unsafe_path");
    assert_eq!(fs::read_dir(&managed).unwrap().count(), 0);
    let mut value = js();
    value["entry"] = json!("linked");
    write_package(&source, &value);
    assert_eq!(inspect(&source).unwrap_err().code, "unsafe_path");
    symlink(&source, temp.path().join("root-link")).unwrap();
    assert_eq!(
        inspect(temp.path().join("root-link")).unwrap_err().code,
        "unsafe_path"
    );
}

#[test]
fn registry_defaults_are_trust_sensitive_and_persisted() {
    let temp = TempDir::new().unwrap();
    write_package(temp.path(), &js());
    let js = inspect(temp.path()).unwrap();
    let provider_value = provider(temp.path());
    write_package(temp.path(), &provider_value);
    let provider = inspect(temp.path()).unwrap();
    let database = Database::in_memory().unwrap();
    let mut registry = Registry::load(&database).unwrap();
    assert!(registry.is_enabled(&js, PackageOrigin::Bundled));
    assert!(!registry.is_enabled(&js, PackageOrigin::Managed));
    assert!(!registry.is_enabled(&provider, PackageOrigin::Bundled));
    assert!(!registry.is_enabled(&provider, PackageOrigin::Managed));
    registry.set_enabled(&provider.manifest.id, true).unwrap();
    registry.set_enabled(&js.manifest.id, false).unwrap();
    registry.save(&database).unwrap();
    let loaded = Registry::load(&database).unwrap();
    assert!(loaded.is_enabled(&provider, PackageOrigin::Managed));
    assert!(!loaded.is_enabled(&js, PackageOrigin::Bundled));
    assert!(InventoryEntry::new(&provider, PackageOrigin::Managed, &loaded).restart_required);
    assert_eq!(
        database
            .namespace("app.extensions")
            .unwrap()
            .get("enabled")
            .unwrap(),
        Some(json!({"example.provider":true, "example.quote":false}))
    );
}
