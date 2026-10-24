//! Local package inventory and installation. Inspection never executes provider code.
use std::{collections::HashSet, path::PathBuf, sync::Arc};

use tradeslope_package_sdk::{Package, PackageKind, PackageOrigin, Registry};
use tradeslope_sdk::{
    Backend, BackendHost,
    runtime::{BackendRuntime, Database},
};

use crate::extensions::{Extensions, JavaScriptCard, data_directory};

#[derive(Clone)]
pub struct InstalledExtension {
    pub id: String,
    pub title: String,
    pub kind: &'static str,
    pub path: PathBuf,
    pub enabled: bool,
    pub loaded: bool,
    pub running: bool,
}

pub struct ExtensionManager {
    pub database: Database,
    pub managed_root: PathBuf,
    db_path: PathBuf,
    roots: Vec<PathBuf>,
    bundled_root: PathBuf,
}

pub struct ExtensionSystem {
    pub registry: Extensions,
    pub host: BackendHost,
    pub runtime: Option<Arc<std::sync::Mutex<BackendRuntime>>>,
    pub manager: Option<Arc<ExtensionManager>>,
    pub error: Option<String>,
}

impl ExtensionSystem {
    pub fn load() -> Self {
        match ExtensionManager::open().and_then(|manager| manager.load()) {
            Ok(system) => system,
            Err(error) => Self {
                registry: Extensions::default(),
                host: BackendHost::new(vec![]).expect("empty backend registry"),
                runtime: None,
                manager: None,
                error: Some(error),
            },
        }
    }
}

impl ExtensionManager {
    fn open() -> Result<Self, String> {
        let directory = data_directory()?;
        std::fs::create_dir_all(&directory).map_err(|_| "Cannot create the app data directory")?;
        let db_path = directory.join("runtime.sqlite3");
        let database = Database::open(&db_path).map_err(|error| error.to_string())?;
        let managed_root = directory.join("extensions");
        std::fs::create_dir_all(&managed_root)
            .map_err(|_| "Cannot create the extension directory")?;
        let bundled_root = std::env::var_os("TRADESLOPE_EXTENSIONS_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|path| path.parent().map(|parent| parent.join("extensions")))
                    .filter(|path| path.is_dir())
            })
            .unwrap_or_else(|| {
                let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
                let staged = root.join("dist/extensions");
                if staged.is_dir() {
                    staged
                } else {
                    root.join("extensions")
                }
            });
        let roots = vec![bundled_root.clone(), managed_root.clone()];
        Ok(Self {
            database,
            managed_root,
            db_path,
            roots,
            bundled_root,
        })
    }

    fn packages(&self) -> (Vec<Package>, Vec<String>) {
        let mut packages = vec![];
        let mut errors = vec![];
        let roots: Vec<_> = self
            .roots
            .iter()
            .filter(|root| root.is_dir())
            .cloned()
            .collect();
        for result in tradeslope_package_sdk::discover(&roots) {
            match result {
                Ok(package) => packages.push(package),
                Err(error) => errors.push(error.to_string()),
            }
        }
        (packages, errors)
    }

    fn origin(&self, package: &Package) -> PackageOrigin {
        if self
            .bundled_root
            .canonicalize()
            .ok()
            .is_some_and(|root| package.root.starts_with(root))
        {
            PackageOrigin::Bundled
        } else {
            PackageOrigin::Managed
        }
    }

    fn load(self) -> Result<ExtensionSystem, String> {
        let flags = Registry::load(&self.database).map_err(|error| error.to_string())?;
        let (packages, errors) = self.packages();
        let mut registry = Extensions {
            errors,
            ..Extensions::default()
        };
        let mut providers: Vec<Arc<dyn Backend>> = vec![];
        for package in packages {
            match &package.manifest.kind {
                PackageKind::Provider {
                    command,
                    args,
                    backend,
                    sources,
                    settings,
                } => {
                    let provider = tradeslope_provider_process::ProcessProvider::new(
                        tradeslope_provider_process::ProcessProviderConfig {
                            package_root: package.root.clone(),
                            manifest: backend.clone(),
                            sources: sources.clone(),
                            settings: settings.clone(),
                            command: PathBuf::from(command),
                            args: args.clone(),
                            db_path: self.db_path.clone(),
                        },
                    )
                    .map_err(|error| error.to_string())?
                    .with_background_sync(true);
                    providers.push(Arc::new(provider));
                }
                PackageKind::JavaScript { entry, card } => {
                    if flags.is_enabled(&package, self.origin(&package)) {
                        registry.register_javascript(JavaScriptCard {
                            manifest: card.clone(),
                            directory: Some(package.root.clone()),
                            entry: entry.clone(),
                        })?;
                    }
                }
            }
        }
        let (runtime, startup_errors) =
            BackendRuntime::new_resilient(self.database.clone(), providers)
                .map_err(|error| error.to_string())?;
        registry
            .errors
            .extend(startup_errors.into_iter().map(|error| error.to_string()));
        let host = BackendHost::new(runtime.backends()).map_err(|error| error.to_string())?;
        Ok(ExtensionSystem {
            registry,
            host,
            runtime: Some(Arc::new(std::sync::Mutex::new(runtime))),
            manager: Some(Arc::new(self)),
            error: None,
        })
    }

    pub fn inventory(
        &self,
        runtime: Option<&BackendRuntime>,
        loaded_cards: &HashSet<String>,
    ) -> Result<Vec<InstalledExtension>, String> {
        let flags = Registry::load(&self.database).map_err(|error| error.to_string())?;
        let states = runtime.map(BackendRuntime::states).unwrap_or_default();
        let (packages, _) = self.packages();
        Ok(packages
            .into_iter()
            .map(|package| {
                let (kind, enabled, loaded) = match &package.manifest.kind {
                    PackageKind::Provider { .. } => {
                        let state = states.iter().find(|state| state.id == package.manifest.id);
                        (
                            "Provider executable",
                            state.is_some_and(|state| state.enabled),
                            state.is_some(),
                        )
                    }
                    PackageKind::JavaScript { .. } => (
                        "Web card",
                        flags.is_enabled(&package, self.origin(&package)),
                        loaded_cards.contains(&package.manifest.id),
                    ),
                };
                InstalledExtension {
                    running: states
                        .iter()
                        .any(|state| state.id == package.manifest.id && state.running),
                    id: package.manifest.id,
                    title: package.manifest.title,
                    kind,
                    path: package.root,
                    enabled,
                    loaded,
                }
            })
            .collect())
    }

    pub fn inspect(&self, path: &str) -> Result<Package, String> {
        tradeslope_package_sdk::inspect(PathBuf::from(path)).map_err(|error| error.to_string())
    }

    pub fn install(&self, path: &str, registered: &HashSet<String>) -> Result<Package, String> {
        let candidate = self.inspect(path)?;
        if registered.contains(&candidate.manifest.id)
            || self
                .packages()
                .0
                .iter()
                .any(|package| package.manifest.id == candidate.manifest.id)
        {
            return Err("An extension with this ID is already present. Existing packages are never overwritten.".into());
        }
        // Revoke both kinds of stale ID-based consent BEFORE the atomic publish.
        // Failure leaves no newly discoverable executable with inherited trust.
        self.database
            .revoke_backend_consent(&candidate.manifest.id)
            .map_err(|error| error.to_string())?;
        self.set_frontend_enabled(&candidate.manifest.id, false)?;
        tradeslope_package_sdk::install(&candidate.root, &self.managed_root)
            .map_err(|error| error.to_string())
    }

    pub fn set_frontend_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let mut registry = Registry::load(&self.database).map_err(|error| error.to_string())?;
        registry
            .set_enabled(id, enabled)
            .map_err(|error| error.to_string())?;
        registry
            .save(&self.database)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manager(root: &std::path::Path) -> ExtensionManager {
        let managed_root = root.join("managed");
        let bundled_root = root.join("bundled");
        std::fs::create_dir_all(&managed_root).unwrap();
        std::fs::create_dir_all(&bundled_root).unwrap();
        let db_path = root.join("runtime.sqlite3");
        ExtensionManager {
            database: Database::open(&db_path).unwrap(),
            managed_root: managed_root.clone(),
            db_path,
            roots: vec![bundled_root.clone(), managed_root],
            bundled_root,
        }
    }

    fn frontend(root: &std::path::Path) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("index.html"), "<html>external</html>").unwrap();
        std::fs::write(root.join("extension.json"), json!({
            "format_version":1,"id":"external.card","title":"External","version":"1",
            "kind":"javascript","entry":"index.html",
            "card":{"id":"external.card","title":"External","renderer":"java_script","inputs":[]}
        }).to_string()).unwrap();
    }

    #[test]
    fn fresh_frontend_install_revokes_stale_preference_before_discovery() {
        let temp = tempfile::tempdir().unwrap();
        let manager = manager(temp.path());
        let source = temp.path().join("source");
        frontend(&source);
        manager.set_frontend_enabled("external.card", true).unwrap();
        manager
            .install(source.to_str().unwrap(), &HashSet::new())
            .unwrap();
        let system = manager.load().unwrap();
        assert!(system.registry.javascript.is_empty());
        assert!(system.registry.errors.is_empty());
    }

    #[test]
    fn removed_but_registered_package_cannot_be_replaced_in_running_app() {
        let temp = tempfile::tempdir().unwrap();
        let manager = manager(temp.path());
        let source = temp.path().join("source");
        frontend(&source);
        let registered = HashSet::from(["external.card".to_string()]);
        assert!(
            manager
                .install(source.to_str().unwrap(), &registered)
                .is_err()
        );
        assert!(!manager.managed_root.join("external.card").exists());
    }

    #[cfg(unix)]
    #[test]
    fn replacement_provider_is_not_started_with_prior_ids_consent() {
        use std::os::unix::fs::PermissionsExt;
        struct OldProvider;
        impl Backend for OldProvider {
            fn manifest(&self) -> tradeslope_sdk::BackendManifest {
                tradeslope_sdk::BackendManifest {
                    id: "external.provider".into(),
                    title: "Old".into(),
                    version: "1".into(),
                    operations: vec![],
                }
            }
            fn execute(
                &self,
                _: &str,
                _: serde_json::Value,
            ) -> tradeslope_sdk::BackendResult<serde_json::Value> {
                unreachable!()
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let manager = manager(temp.path());
        {
            let mut old =
                BackendRuntime::new(manager.database.clone(), vec![Arc::new(OldProvider)]).unwrap();
            old.set_enabled("external.provider", true).unwrap();
        }
        let source = temp.path().join("replacement");
        std::fs::create_dir_all(source.join("bin")).unwrap();
        let executable = source.join("bin/provider");
        std::fs::write(&executable, "#!/bin/sh\ntouch launched\n").unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(source.join("extension.json"), json!({
            "format_version":1,"id":"external.provider","title":"Replacement","version":"2",
            "kind":"provider","command":"bin/provider","args":[],
            "backend":{"id":"external.provider","title":"Replacement","version":"2","operations":[]},
            "sources":[],"settings":{"schema":{"type":"object","fields":{}},"defaults":{}}
        }).to_string()).unwrap();
        let installed = manager
            .install(source.to_str().unwrap(), &HashSet::new())
            .unwrap();
        let system = manager.load().unwrap();
        let runtime = system.runtime.unwrap();
        assert!(!runtime.lock().unwrap().states()[0].enabled);
        assert!(!installed.root.join("launched").exists());
    }
}
