use super::*;
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);

/// Local-directory installation only. Never overwrites a package. Staging and
/// commit are on the managed filesystem; SDK installers serialize per ID.
/// Like inspection, this assumes no hostile concurrent filesystem mutations.
pub fn install(source_dir: impl AsRef<Path>, managed_root: impl AsRef<Path>) -> Result<Package> {
    let source = inspect(source_dir)?;
    fs::create_dir_all(managed_root.as_ref())?;
    let managed = managed_root.as_ref().canonicalize()?;
    if managed.starts_with(&source.root) {
        return Err(error(
            "unsafe_path",
            "Managed root must not be inside the source package",
        ));
    }
    let destination = managed.join(&source.manifest.id);
    let lock_path = managed.join(format!(".install-{}.lock", source.manifest.id));
    let lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .map_err(|_| error("install_busy", "Another install holds this package ID lock"))?;
    let _lock = Cleanup {
        path: lock_path,
        directory: false,
    };
    let _lock_file = lock;
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(error(
            "already_installed",
            "Package ID is already installed; uninstall explicitly before replacing",
        ));
    }
    let stage = loop {
        let path = managed.join(format!(
            ".stage-{}-{}",
            std::process::id(),
            NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&path) {
            Ok(()) => break path,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    let _stage = Cleanup {
        path: stage.clone(),
        directory: true,
    };
    let mut budget = Budget::default();
    copy_tree(&source.root, &stage, &mut budget, 0)?;
    let staged = inspect(&stage)?;
    if staged.manifest.id != source.manifest.id {
        return Err(error("source_changed", "Package ID changed while copying"));
    }
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(error(
            "already_installed",
            "Package ID was installed while copying",
        ));
    }
    fs::rename(&stage, &destination)?;
    // No fallible post-commit inspection: staged metadata is already validated.
    Ok(Package {
        root: destination,
        manifest: staged.manifest,
    })
}

struct Cleanup {
    path: PathBuf,
    directory: bool,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.directory {
            let _ = fs::remove_dir_all(&self.path);
        } else {
            let _ = fs::remove_file(&self.path);
        }
    }
}
#[derive(Default)]
struct Budget {
    entries: usize,
    bytes: u64,
}

fn excluded(name: &str) -> bool {
    matches!(name, ".git" | "target" | "node_modules")
}
fn private_file(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == ".env"
        || name.starts_with(".env.")
        || matches!(
            name.as_str(),
            "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519"
        )
        || [".pem", ".key", ".p12", ".pfx"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn copy_tree(source: &Path, target: &Path, budget: &mut Budget, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err(error(
            "size_limit",
            "Package directory nesting exceeds 64 levels",
        ));
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let text = name
            .to_str()
            .ok_or_else(|| error("unsafe_path", "Package filenames must be UTF-8"))?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Err(error("unsafe_path", "Cannot install symlinks"));
        }
        if excluded(text) {
            continue;
        }
        if private_file(text) {
            return Err(error(
                "private_file",
                "Remove environment files and private key files before installing",
            ));
        }
        // Count directories as well as files, bounding empty-directory attacks.
        budget.entries += 1;
        if budget.entries > MAX_FILES {
            return Err(error(
                "size_limit",
                "Package exceeds 4096 filesystem entries",
            ));
        }
        let destination = target.join(&name);
        if metadata.is_dir() {
            fs::create_dir(&destination)?;
            copy_tree(&entry.path(), &destination, budget, depth + 1)?;
        } else if metadata.is_file() {
            let remaining = MAX_FILE_BYTES.min(MAX_TOTAL_BYTES.saturating_sub(budget.bytes));
            if metadata.len() > remaining {
                return Err(error(
                    "size_limit",
                    "Package exceeds file or total byte limit",
                ));
            }
            let mut input = fs::File::open(entry.path())?.take(remaining + 1);
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            let mut buffer = [0u8; 64 * 1024];
            let mut copied = 0u64;
            loop {
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                copied += count as u64;
                if copied > remaining {
                    return Err(error(
                        "size_limit",
                        "Package file grew beyond its byte limit",
                    ));
                }
                output.write_all(&buffer[..count])?;
            }
            budget.bytes += copied;
            // Retain executable bits, but never setuid/setgid/sticky privileges.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &destination,
                    fs::Permissions::from_mode(metadata.permissions().mode() & 0o777),
                )?;
            }
        } else {
            return Err(error(
                "unsafe_path",
                "Only regular files and directories can be installed",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cumulative_byte_budget_is_enforced_without_large_allocations() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(source.join("small"), [0u8; 8]).unwrap();
        let mut budget = Budget {
            entries: 0,
            bytes: MAX_TOTAL_BYTES - 4,
        };
        assert_eq!(
            copy_tree(&source, &target, &mut budget, 0)
                .unwrap_err()
                .code,
            "size_limit"
        );
        assert!(!target.join("small").exists());
    }
}
