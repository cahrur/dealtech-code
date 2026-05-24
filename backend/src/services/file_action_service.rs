use std::path::{Component, Path, PathBuf};

use tokio::fs;

use crate::error::{AppError, Result};

pub async fn write_file(worktree_path: &PathBuf, relative_path: &str, content: &str) -> Result<PathBuf> {
    let target = resolve_safe_path(worktree_path, relative_path)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir parent: {}", e)))?;
    }
    fs::write(&target, content)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("write file: {}", e)))?;
    Ok(target)
}

fn resolve_safe_path(worktree_path: &Path, relative_path: &str) -> Result<PathBuf> {
    let rel = Path::new(relative_path);
    if rel.is_absolute() {
        return Err(AppError::BadRequest("absolute paths are not allowed".to_string()));
    }

    for component in rel.components() {
        if matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_)) {
            return Err(AppError::BadRequest("unsafe path outside workspace".to_string()));
        }
    }

    Ok(worktree_path.join(rel))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::resolve_safe_path;

    #[test]
    fn rejects_parent_escape() {
        let base = PathBuf::from("/tmp/worktree");
        assert!(resolve_safe_path(&base, "../secret.txt").is_err());
    }

    #[test]
    fn resolves_normal_relative_path() {
        let base = PathBuf::from("/tmp/worktree");
        let path = resolve_safe_path(&base, "README.md").expect("path should be valid");
        assert_eq!(path, base.join("README.md"));
    }
}
