#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use anyhow::Context;

use crate::error::ToolError;

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
}
