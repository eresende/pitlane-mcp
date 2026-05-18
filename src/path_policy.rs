#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use anyhow::Context;

use crate::error::ToolError;

/// Return metadata only for ordinary files, without following symlinks.
///
/// WalkDir is configured with `follow_links(false)`, but `Path::is_file()` and
/// `std::fs::metadata()` still follow file symlinks. Use this helper at read
/// boundaries so a symlink inside an allowed project cannot expose a file
/// outside that project.
pub fn regular_file_metadata(path: &Path) -> anyhow::Result<Option<std::fs::Metadata>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("Cannot inspect file: {}", path.display()));
        }
    };

    if metadata.file_type().is_file() {
        Ok(Some(metadata))
    } else {
        Ok(None)
    }
}

pub fn is_regular_file(path: &Path) -> bool {
    matches!(regular_file_metadata(path), Ok(Some(_)))
}

/// Return a canonical path for an ordinary file, without allowing the final
/// path component to be a symlink.
pub fn canonical_regular_file(path: &Path) -> anyhow::Result<Option<PathBuf>> {
    if regular_file_metadata(path)?.is_none() {
        return Ok(None);
    }
    Ok(Some(path.canonicalize().with_context(|| {
        format!("Cannot canonicalize file: {}", path.display())
    })?))
}

/// Open an ordinary source file without following a final symlink component.
///
/// This closes the check-then-open race for source reads on supported platforms:
/// the open itself refuses or preserves final-component symlinks, then the
/// opened handle is checked before any bytes are read.
pub fn open_regular_file(path: &Path) -> anyhow::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .with_context(|| format!("Cannot open regular file: {}", path.display()))?;
        if !file.metadata()?.file_type().is_file() {
            anyhow::bail!("Refusing to read non-regular file: {}", path.display());
        }
        Ok(file)
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .with_context(|| format!("Cannot open regular file: {}", path.display()))?;
        let file_type = file.metadata()?.file_type();
        if !file_type.is_file() || file_type.is_symlink() {
            anyhow::bail!("Refusing to read non-regular file: {}", path.display());
        }
        Ok(file)
    }

    #[cfg(not(any(unix, windows)))]
    {
        if regular_file_metadata(path)?.is_none() {
            anyhow::bail!("Refusing to read non-regular file: {}", path.display());
        }
        let file = std::fs::File::open(path)
            .with_context(|| format!("Cannot open regular file: {}", path.display()))?;
        if !file.metadata()?.file_type().is_file() {
            anyhow::bail!("Refusing to read non-regular file: {}", path.display());
        }
        Ok(file)
    }
}

pub fn read_regular_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;

    let mut file = open_regular_file(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn configured_allowed_roots() -> anyhow::Result<Option<Vec<PathBuf>>> {
    #[cfg(test)]
    let raw = test_allowed_roots_override().with(|slot| slot.borrow().clone());
    #[cfg(not(test))]
    let raw = std::env::var_os("PITLANE_ALLOWED_ROOTS");

    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }

    let mut roots = Vec::new();
    for root in std::env::split_paths(&raw) {
        if root.as_os_str().is_empty() {
            continue;
        }
        let canonical = root.canonicalize().with_context(|| {
            format!(
                "Cannot canonicalize root from PITLANE_ALLOWED_ROOTS: {}",
                root.display()
            )
        })?;
        roots.push(canonical);
    }

    if roots.is_empty() {
        Ok(None)
    } else {
        Ok(Some(roots))
    }
}

fn ensure_allowed_root(path: &Path) -> anyhow::Result<()> {
    let Some(roots) = configured_allowed_roots()? else {
        return Ok(());
    };

    if roots.iter().any(|root| path.starts_with(root)) {
        return Ok(());
    }

    Err(ToolError::AccessDenied {
        path: path.display().to_string(),
    }
    .into())
}

/// Format a file path relative to the project root for agent-facing summaries.
pub fn display_path_relative_to_project(project_root: &Path, file: &Path) -> String {
    let file = file.to_string_lossy().replace('\\', "/");
    let root = project_root
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();

    if file == root {
        return ".".to_string();
    }

    let prefix = format!("{root}/");
    if let Some(rest) = file.strip_prefix(&prefix) {
        return rest.to_string();
    }

    file
}

pub fn resolve_project_path(project: &str) -> anyhow::Result<PathBuf> {
    let canonical = Path::new(project)
        .canonicalize()
        .with_context(|| format!("Cannot canonicalize path: {}", project))?;
    ensure_allowed_root(&canonical)?;
    Ok(canonical)
}

pub fn resolve_project_file(project_root: &Path, file_path: &str) -> anyhow::Result<PathBuf> {
    let requested = Path::new(file_path);
    if requested.is_absolute() {
        return Err(ToolError::AccessDenied {
            path: file_path.to_string(),
        }
        .into());
    }

    let mut resolved = PathBuf::from(project_root);
    for component in requested.components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if resolved == project_root {
                    return Err(ToolError::AccessDenied {
                        path: file_path.to_string(),
                    }
                    .into());
                }
                resolved.pop();
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(ToolError::AccessDenied {
                    path: file_path.to_string(),
                }
                .into());
            }
        }
    }

    if resolved.exists() {
        let canonical = resolved
            .canonicalize()
            .with_context(|| format!("Cannot canonicalize path: {}", resolved.display()))?;
        if !canonical.starts_with(project_root) {
            return Err(ToolError::AccessDenied {
                path: file_path.to_string(),
            }
            .into());
        }
        Ok(canonical)
    } else {
        Ok(resolved)
    }
}

#[cfg(test)]
thread_local! {
    static TEST_ALLOWED_ROOTS_OVERRIDE: RefCell<Option<OsString>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_allowed_roots(value: Option<OsString>) {
    TEST_ALLOWED_ROOTS_OVERRIDE.with(|slot| {
        *slot.borrow_mut() = value;
    });
}

#[cfg(test)]
fn test_allowed_roots_override() -> &'static std::thread::LocalKey<RefCell<Option<OsString>>> {
    &TEST_ALLOWED_ROOTS_OVERRIDE
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    #[test]
    fn display_path_relative_to_project_strips_root_prefix() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("src/lib.rs");
        assert_eq!(display_path_relative_to_project(&root, &file), "src/lib.rs");
    }

    #[test]
    fn resolve_project_path_allows_unset_env() {
        set_test_allowed_roots(None);

        let dir = TempDir::new().unwrap();
        let resolved = resolve_project_path(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(resolved, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn resolve_project_path_rejects_outside_allowed_roots() {
        let allowed = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        set_test_allowed_roots(Some(allowed.path().as_os_str().to_os_string()));

        let err = resolve_project_path(outside.path().to_str().unwrap()).unwrap_err();
        let err = err.downcast::<ToolError>().unwrap();
        assert!(matches!(err, ToolError::AccessDenied { .. }));
        set_test_allowed_roots(None);
    }

    #[test]
    fn resolve_project_file_rejects_absolute_paths() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let err = resolve_project_file(&dir.path().canonicalize().unwrap(), file.to_str().unwrap())
            .unwrap_err();
        let err = err.downcast::<ToolError>().unwrap();
        assert!(matches!(err, ToolError::AccessDenied { .. }));
    }

    #[test]
    fn resolve_project_file_rejects_parent_escape() {
        let dir = TempDir::new().unwrap();
        let err =
            resolve_project_file(&dir.path().canonicalize().unwrap(), "../secret.rs").unwrap_err();
        let err = err.downcast::<ToolError>().unwrap();
        assert!(matches!(err, ToolError::AccessDenied { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn regular_file_metadata_rejects_file_symlink() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target.rs");
        let link = dir.path().join("link.rs");
        std::fs::write(&target, "fn target() {}\n").unwrap();
        symlink(&target, &link).unwrap();

        assert!(regular_file_metadata(&target).unwrap().is_some());
        assert!(regular_file_metadata(&link).unwrap().is_none());
        assert!(!is_regular_file(&link));
    }

    #[cfg(unix)]
    #[test]
    fn open_regular_file_rejects_in_project_file_symlink() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target.rs");
        let link = dir.path().join("link.rs");
        std::fs::write(&target, "fn target() {}\n").unwrap();
        symlink("target.rs", &link).unwrap();

        assert!(read_regular_file(&target).is_ok());
        assert!(read_regular_file(&link).is_err());
    }
}
