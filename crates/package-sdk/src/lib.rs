//! Declarative package inspection. No provider process is ever launched here.
//!
//! Enablement is a startup preference, not permission to execute during discovery.
//! The application owns process adapters, web resource MIME handling and restarts.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tradeslope_sdk::{
    Backend, BackendError, BackendHost, BackendManifest, BackendResult, CardManifest, DataSource,
    ExtensionManifest, ExtensionRegistry, Renderer, Schema,
    runtime::{Database, ExtensionSettings},
};

mod install;
pub use install::install;

pub type Result<T> = std::result::Result<T, PackageError>;
pub const MAX_FILES: usize = 4096;
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_FRONTEND_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct PackageError {
    pub code: &'static str,
    pub message: String,
    pub path: Option<PathBuf>,
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(path) = &self.path {
            write!(f, " ({})", path.display())?;
        }
        Ok(())
    }
}
impl std::error::Error for PackageError {}
impl From<std::io::Error> for PackageError {
    fn from(_: std::io::Error) -> Self {
        error("io_error", "Package filesystem operation failed")
    }
}
impl From<BackendError> for PackageError {
    fn from(value: BackendError) -> Self {
        error("invalid_metadata", value.to_string())
    }
}
fn error(code: &'static str, message: impl Into<String>) -> PackageError {
    PackageError {
        code,
        message: message.into(),
        path: None,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageManifest {
    pub format_version: u32,
    pub id: String,
    pub title: String,
    pub version: String,
    #[serde(flatten)]
    pub kind: PackageKind,
}

/// `command` and `entry` are package-relative paths, not shell commands or URLs.
/// Provider arguments are public, static strings; credentials belong in env refs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PackageKind {
    #[serde(rename = "provider")]
    Provider {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        backend: BackendManifest,
        #[serde(default)]
        sources: Vec<DataSource>,
        #[serde(default)]
        settings: ExtensionSettings,
    },
    #[serde(rename = "javascript")]
    JavaScript { entry: String, card: CardManifest },
}

impl PackageKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Provider { .. } => "provider",
            Self::JavaScript { .. } => "javascript",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Package {
    pub root: PathBuf,
    pub manifest: PackageManifest,
}

impl Package {
    /// Resolve a relative regular file, rejecting symlinks in every component.
    pub fn resolve_file(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        contained_file(&self.root, relative.as_ref())
    }

    /// Read a bounded UTF-8 frontend resource. MIME selection belongs to the UI.
    pub fn read_resource(&self, relative: impl AsRef<Path>) -> Result<String> {
        if !matches!(self.manifest.kind, PackageKind::JavaScript { .. }) {
            return Err(error(
                "invalid_kind",
                "Only JavaScript packages have frontend resources",
            ));
        }
        let bytes = read_bounded(&self.resolve_file(relative)?, MAX_FRONTEND_BYTES)?;
        String::from_utf8(bytes)
            .map_err(|_| error("invalid_utf8", "Frontend resource must be UTF-8"))
    }
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 100
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        || !id.as_bytes()[0].is_ascii_alphanumeric()
        || id.ends_with('.')
        || id.contains("..")
    {
        return Err(error(
            "invalid_id",
            "ID must be 1–100 lowercase ASCII letters, digits, dots, underscores or hyphens, starting with a letter or digit",
        ));
    }
    // Also safe as directory names on Windows.
    let stem = id.split('.').next().unwrap_or_default();
    if matches!(stem, "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        return Err(error("invalid_id", "ID is a reserved filesystem name"));
    }
    Ok(())
}

fn contained_file(root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.as_os_str().is_empty()
        || relative
            .to_str()
            .is_none_or(|s| s.contains('\\') || s.contains(':'))
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(error(
            "unsafe_path",
            "Expected a package-relative path without traversal",
        ));
    }
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(error("unsafe_path", "Package files must not be symlinks"));
        }
    }
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return Err(error(
            "unsafe_path",
            "Package path must name a contained regular file",
        ));
    }
    Ok(canonical)
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > limit {
        return Err(error("size_limit", "Package file exceeds its size limit"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(error("size_limit", "Package file exceeds its size limit"));
    }
    Ok(bytes)
}

/// Inspect a package directory without loading code, starting services or reading
/// the environment. Legacy schema_version=2 JavaScript manifests are normalized.
pub fn inspect(root: impl AsRef<Path>) -> Result<Package> {
    let original = root.as_ref();
    let result = (|| {
        if fs::symlink_metadata(original)?.file_type().is_symlink() {
            return Err(error("unsafe_path", "Package root must not be a symlink"));
        }
        let root = original.canonicalize()?;
        let bytes = read_bounded(
            &contained_file(&root, Path::new("extension.json"))?,
            MAX_FRONTEND_BYTES,
        )?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| error("invalid_manifest", "Invalid extension.json JSON"))?;
        if value.get("kind").and_then(|v| v.as_str()) == Some("native_source") {
            return Err(error(
                "rebuild_required",
                "Native source packages require rebuilding the application; binary GPUI hot-loading is unsupported",
            ));
        }
        let manifest: PackageManifest = if value.get("format_version").is_none()
            && value.get("schema_version").and_then(|v| v.as_u64()) == Some(2)
        {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Legacy {
                schema_version: u32,
                entry: String,
                card: CardManifest,
            }
            let legacy: Legacy = serde_json::from_value(value)
                .map_err(|_| error("invalid_manifest", "Invalid legacy JavaScript manifest"))?;
            debug_assert_eq!(legacy.schema_version, 2);
            PackageManifest {
                format_version: 1,
                id: legacy.card.id.clone(),
                title: legacy.card.title.clone(),
                version: "1".into(),
                kind: PackageKind::JavaScript {
                    entry: legacy.entry,
                    card: legacy.card,
                },
            }
        } else {
            // Reject unknown top-level fields, notably command/args in JS packages.
            let kind = value
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let specific: &[&str] = match kind {
                "provider" => &["command", "args", "backend", "sources", "settings"],
                "javascript" => &["entry", "card"],
                _ => {
                    return Err(error(
                        "invalid_kind",
                        "Only provider and javascript packages can be hot-loaded",
                    ));
                }
            };
            if value.as_object().is_none_or(|v| {
                v.keys().any(|key| {
                    !["format_version", "id", "title", "version", "kind"].contains(&key.as_str())
                        && !specific.contains(&key.as_str())
                })
            }) {
                return Err(error(
                    "invalid_manifest",
                    "Unknown or inappropriate package fields",
                ));
            }
            serde_json::from_value(value)
                .map_err(|_| error("invalid_manifest", "Invalid package manifest fields"))?
        };
        validate_manifest(&manifest, &root)?;
        Ok(Package { root, manifest })
    })();
    result.map_err(|mut e: PackageError| {
        e.path = Some(original.to_path_buf());
        e
    })
}

/// SDK registration runs against this metadata-only dummy. Even accidental
/// future requests cannot execute provider code.
struct Metadata {
    backend: BackendManifest,
    sources: Vec<DataSource>,
}
impl Backend for Metadata {
    fn manifest(&self) -> BackendManifest {
        self.backend.clone()
    }
    fn sources(&self) -> Vec<DataSource> {
        self.sources.clone()
    }
    fn execute(&self, _: &str, _: serde_json::Value) -> BackendResult<serde_json::Value> {
        Err(BackendError::new(
            "metadata_only",
            "Package inspection never executes providers",
        ))
    }
}

fn validate_manifest(manifest: &PackageManifest, root: &Path) -> Result<()> {
    if manifest.format_version != 1 {
        return Err(error(
            "unsupported_version",
            "Only package format_version 1 is supported",
        ));
    }
    validate_id(&manifest.id)?;
    if manifest.title.trim().is_empty() || manifest.version.trim().is_empty() {
        return Err(error(
            "invalid_manifest",
            "Package title and version must not be empty",
        ));
    }
    match &manifest.kind {
        PackageKind::Provider {
            command,
            args,
            backend,
            sources,
            settings,
        } => {
            if backend.id != manifest.id {
                return Err(error("id_mismatch", "Backend ID must equal package ID"));
            }
            if backend.title.trim().is_empty() {
                return Err(error("invalid_metadata", "Backend title must not be empty"));
            }
            let executable = contained_file(root, Path::new(command))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if executable.metadata()?.permissions().mode() & 0o111 == 0 {
                    return Err(error(
                        "not_executable",
                        "Provider command must be an executable file",
                    ));
                }
            }
            #[cfg(not(unix))]
            let _ = executable;
            if args.iter().any(|arg| arg.contains(['\0', '\n', '\r'])) {
                return Err(error(
                    "invalid_args",
                    "Provider arguments must be static scalar strings without control separators",
                ));
            }
            for op in &backend.operations {
                validate_schema(&op.parameters)?;
                validate_schema(&op.output)?;
            }
            for source in sources {
                validate_schema(&source.configuration)?;
            }
            validate_schema(&settings.schema)?;
            if settings.schema.fields().is_none() {
                return Err(error(
                    "invalid_settings",
                    "Settings schema must be an object",
                ));
            }
            validate_settings_value(&settings.schema, &settings.defaults)?;
            // Host construction validates operation IDs, source schemas and
            // source/operation compatibility, using no executable adapter.
            drop(BackendHost::new(vec![Arc::new(Metadata {
                backend: backend.clone(),
                sources: sources.clone(),
            })])?);
        }
        PackageKind::JavaScript { entry, card } => {
            if card.id != manifest.id {
                return Err(error("id_mismatch", "Card ID must equal package ID"));
            }
            if card.renderer != Renderer::JavaScript {
                return Err(error(
                    "invalid_renderer",
                    "JavaScript packages require the java_script renderer",
                ));
            }
            let bytes = read_bounded(&contained_file(root, Path::new(entry))?, MAX_FRONTEND_BYTES)?;
            std::str::from_utf8(&bytes)
                .map_err(|_| error("invalid_utf8", "Frontend entry must be UTF-8"))?;
            for port in &card.inputs {
                validate_schema(&port.parameters)?;
                validate_schema(&port.output)?;
            }
            ExtensionRegistry::new(vec![ExtensionManifest {
                id: manifest.id.clone(),
                title: manifest.title.clone(),
                version: manifest.version.clone(),
                cards: vec![card.clone()],
            }])?;
        }
    }
    Ok(())
}

// Schema deserialization is structural; additionally reject unusable empty
// choices and validate field defaults even if no runtime value uses that field.
fn validate_schema(schema: &Schema) -> Result<()> {
    match schema {
        Schema::Enum { values } if values.is_empty() => {
            return Err(error("invalid_schema", "Enum schema must have values"));
        }
        Schema::Union { variants } if variants.is_empty() => {
            return Err(error("invalid_schema", "Union schema must have variants"));
        }
        Schema::Object { fields } => {
            for field in fields.values() {
                validate_schema(&field.schema)?;
                if let Some(default) = &field.default {
                    field.schema.validate(default)?;
                }
            }
        }
        Schema::Union { variants } => {
            for schema in variants {
                validate_schema(schema)?;
            }
        }
        Schema::Tuple { items } => {
            for schema in items {
                validate_schema(schema)?;
            }
        }
        Schema::Array { items } => validate_schema(items)?,
        Schema::Nullable { inner } => validate_schema(inner)?,
        _ => (),
    }
    Ok(())
}

// SDK settings use strict parameter semantics (unlike permissive output value
// validation). The SDK's strict validator is crate-private, so check extra keys
// here while using its public validator for all actual value types.
fn validate_settings_value(schema: &Schema, value: &serde_json::Value) -> Result<()> {
    schema.validate(value)?;
    match schema {
        Schema::Object { fields } => {
            for (key, value) in value.as_object().expect("validated object") {
                let field = fields.get(key).ok_or_else(|| {
                    error("invalid_settings", "Settings contain undeclared fields")
                })?;
                validate_settings_value(&field.schema, value)?;
            }
        }
        Schema::Array { items } => {
            for value in value.as_array().expect("validated array") {
                validate_settings_value(items, value)?;
            }
        }
        Schema::Tuple { items } => {
            for (schema, value) in items.iter().zip(value.as_array().expect("validated tuple")) {
                validate_settings_value(schema, value)?;
            }
        }
        Schema::Nullable { inner } if !value.is_null() => validate_settings_value(inner, value)?,
        Schema::Union { variants }
            if !variants
                .iter()
                .any(|schema| validate_settings_value(schema, value).is_ok()) =>
        {
            return Err(error(
                "invalid_settings",
                "Settings match no strict union variant",
            ));
        }
        _ => (),
    }
    Ok(())
}

/// Search immediate child directories in deterministic root/name order. Missing
/// search roots are empty. Other IO and manifest failures remain visible.
/// Every occurrence of a duplicated ID is rejected, rather than picking a winner.
pub fn discover(roots: &[PathBuf]) -> Vec<Result<Package>> {
    let mut results = Vec::new();
    for root in roots {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                let mut e = error("io_error", "Cannot read package search root");
                e.path = Some(root.clone());
                results.push(Err(e));
                continue;
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    if entry.file_name().to_string_lossy().starts_with('.') {
                        continue;
                    }
                    match entry.file_type() {
                        Ok(t)
                            if (t.is_dir() || t.is_symlink())
                                && path.join("extension.json").is_file() =>
                        {
                            paths.push(path)
                        }
                        Ok(_) => (),
                        Err(e) => results.push(Err(e.into())),
                    }
                }
                Err(e) => results.push(Err(e.into())),
            }
        }
        paths.sort();
        results.extend(paths.into_iter().map(inspect));
    }
    let mut counts = HashMap::new();
    for package in results.iter().flatten() {
        *counts.entry(package.manifest.id.clone()).or_insert(0) += 1;
    }
    for result in &mut results {
        if let Ok(package) = result
            && counts[&package.manifest.id] > 1
        {
            let mut e = error(
                "duplicate_id",
                format!("Duplicate package ID: {}", package.manifest.id),
            );
            e.path = Some(package.root.clone());
            *result = Err(e);
        }
    }
    results
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageOrigin {
    Bundled,
    Managed,
}

/// A single SDK Store value: namespace `app.extensions`, key `enabled`.
/// A provider is disabled by default even when bundled. An explicit saved true
/// represents user trust; callers must never populate it from package metadata.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub enabled: BTreeMap<String, bool>,
}
impl Registry {
    pub fn load(database: &Database) -> Result<Self> {
        let value = database.namespace("app.extensions")?.get("enabled")?;
        let enabled: BTreeMap<String, bool> = match value {
            None => BTreeMap::new(),
            Some(value) => serde_json::from_value(value).map_err(|_| {
                error(
                    "invalid_registry",
                    "Enabled registry must be an ID to boolean map",
                )
            })?,
        };
        for id in enabled.keys() {
            validate_id(id)?;
        }
        Ok(Self { enabled })
    }
    pub fn save(&self, database: &Database) -> Result<()> {
        for id in self.enabled.keys() {
            validate_id(id)?;
        }
        database.namespace("app.extensions")?.set(
            "enabled",
            &serde_json::to_value(&self.enabled)
                .map_err(|_| error("invalid_registry", "Cannot encode registry"))?,
        )?;
        Ok(())
    }
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<()> {
        validate_id(id)?;
        self.enabled.insert(id.to_owned(), enabled);
        Ok(())
    }
    pub fn is_enabled(&self, package: &Package, origin: PackageOrigin) -> bool {
        self.enabled.get(&package.manifest.id).copied().unwrap_or(
            origin == PackageOrigin::Bundled
                && matches!(package.manifest.kind, PackageKind::JavaScript { .. }),
        )
    }
}

#[derive(Clone, Debug)]
pub struct InventoryEntry {
    pub id: String,
    pub title: String,
    pub kind: &'static str,
    pub path: PathBuf,
    pub enabled: bool,
    /// Changes take effect on restart; this is not a pending-change detector.
    pub restart_required: bool,
}
impl InventoryEntry {
    pub fn new(package: &Package, origin: PackageOrigin, registry: &Registry) -> Self {
        Self {
            id: package.manifest.id.clone(),
            title: package.manifest.title.clone(),
            kind: package.manifest.kind.as_str(),
            path: package.root.clone(),
            enabled: registry.is_enabled(package, origin),
            restart_required: true,
        }
    }
}

#[cfg(test)]
mod tests;
