//! Explicit registrations for native crates; optional local JS packages use the
//! same card manifest. Neither path adds exchange-specific branches to the UI.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tradeslope_native_sdk::NativeCardFactory;
use tradeslope_sdk::{CardManifest, Renderer};

use crate::page::CardKind;

pub(crate) fn data_directory() -> Result<PathBuf, String> {
    if let Some(directory) = std::env::var_os("TRADESLOPE_DATA_DIR") {
        if directory.is_empty() {
            return Err("TRADESLOPE_DATA_DIR must not be empty".into());
        }
        return Ok(PathBuf::from(directory));
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|home| PathBuf::from(home).join("TradeSlope"))
            .ok_or_else(|| "LOCALAPPDATA is unavailable; set TRADESLOPE_DATA_DIR".into())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home =
            std::env::var_os("HOME").ok_or("HOME is unavailable; set TRADESLOPE_DATA_DIR")?;
        #[cfg(target_os = "macos")]
        let directory = PathBuf::from(home).join("Library/Application Support/TradeSlope");
        #[cfg(not(target_os = "macos"))]
        let directory = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(home).join(".local/share"))
            .join("TradeSlope");
        Ok(directory)
    }
}

pub struct JavaScriptCard {
    pub manifest: CardManifest,
    pub directory: Option<PathBuf>,
    pub entry: String,
}

#[derive(Default)]
pub struct Extensions {
    pub native: BTreeMap<String, Box<dyn NativeCardFactory>>,
    pub javascript: BTreeMap<String, JavaScriptCard>,
    pub errors: Vec<String>,
}

impl Extensions {
    fn vacant(&self, id: &str) -> Result<(), String> {
        if id.is_empty()
            || id.len() > 100
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b".-_".contains(&c))
        {
            return Err(
                "Extension card ID must contain only lowercase letters, digits, '.', '-' or '_'"
                    .into(),
            );
        }
        if self.native.contains_key(id) || self.javascript.contains_key(id) {
            return Err(format!("Duplicate card ID: {id}"));
        }
        Ok(())
    }

    /// Source-linked GPUI extension hook; runtime packages do not use a Rust ABI.
    #[allow(dead_code)]
    pub fn register_native(&mut self, factory: Box<dyn NativeCardFactory>) -> Result<(), String> {
        let manifest = factory.manifest();
        self.vacant(&manifest.id)?;
        if manifest.renderer != Renderer::Native {
            return Err("Native factory must declare a native renderer".into());
        }
        self.native.insert(manifest.id.clone(), factory);
        Ok(())
    }

    pub fn register_javascript(&mut self, card: JavaScriptCard) -> Result<(), String> {
        self.vacant(&card.manifest.id)?;
        if card.manifest.renderer != Renderer::JavaScript {
            return Err("JavaScript package must declare a JavaScript renderer".into());
        }
        self.javascript.insert(card.manifest.id.clone(), card);
        Ok(())
    }

    pub fn choices(&self) -> Vec<(CardKind, String)> {
        self.native
            .iter()
            .map(|(id, factory)| (CardKind::Native(id.clone()), factory.manifest().title))
            .chain(self.javascript.iter().map(|(id, card)| {
                (
                    CardKind::JavaScript(id.clone()),
                    card.manifest.title.clone(),
                )
            }))
            .collect()
    }

    pub fn title(&self, kind: &CardKind) -> String {
        self.choices()
            .into_iter()
            .find(|(id, _)| id == kind)
            .map(|(_, title)| title)
            .unwrap_or_else(|| kind.clone().title().into())
    }
}

impl Extensions {
    pub fn manifest(&self, kind: &CardKind) -> Option<CardManifest> {
        let id = kind.extension_id()?;
        self.native
            .get(id)
            .map(|factory| factory.manifest())
            .or_else(|| {
                self.javascript
                    .get(id)
                    .map(|package| package.manifest.clone())
            })
    }
}

#[cfg(test)]
fn load_package(directory: &Path) -> Result<JavaScriptCard, String> {
    let package = tradeslope_package_sdk::inspect(directory).map_err(|error| error.to_string())?;
    match package.manifest.kind {
        tradeslope_package_sdk::PackageKind::JavaScript { card, entry } => Ok(JavaScriptCard {
            manifest: card,
            directory: Some(package.root),
            entry,
        }),
        _ => Err("Expected a JavaScript card package".into()),
    }
}

/// Canonical paths prevent a package from serving files outside its root.
pub fn package_file(root: &Path, path: &str) -> Result<PathBuf, String> {
    if path.is_empty()
        || path.contains('\\')
        || Path::new(path).is_absolute()
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err("Invalid package asset path".into());
    }
    let file = root
        .join(path)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !file.starts_with(root) || !file.is_file() {
        return Err("Asset is outside its package".into());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str) -> JavaScriptCard {
        JavaScriptCard {
            manifest: CardManifest {
                id: id.into(),
                title: "Test".into(),
                renderer: Renderer::JavaScript,
                inputs: vec![],
            },
            directory: None,
            entry: "index.html".into(),
        }
    }

    #[test]
    fn registry_rejects_duplicates_and_invalid_ids() {
        let mut registry = Extensions::default();
        registry.register_javascript(card("test.card")).unwrap();
        assert!(registry.register_javascript(card("test.card")).is_err());
        assert!(registry.register_javascript(card("../bad")).is_err());
        assert!(registry.register_javascript(card("UPPER")).is_err());
        assert_eq!(registry.choices().len(), 1);
    }

    #[test]
    fn example_package_is_loadable_and_cannot_escape_root() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/extensions/quote")
            .canonicalize()
            .unwrap();
        let package = load_package(&root).unwrap();
        assert_eq!(package.manifest.id, "example.quote");
        assert!(package_file(&root, "quote.js").is_ok());
        for path in [
            "../quote/index.html",
            "/etc/passwd",
            "%2e%2e/Cargo.toml",
            "x\\y",
        ] {
            assert!(package_file(&root, path).is_err());
        }
    }
}
