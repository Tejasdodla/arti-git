use std::path::{Path, PathBuf};
use std::fs;

use crate::core::{GitError, Result};

/// Normalize a path by handling ".." and "." components
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if !result.as_os_str().is_empty() {
                    result.pop();
                }
            },
            std::path::Component::CurDir => {},
            _ => result.push(component),
        }
    }
    
    result
}

/// Ensure a directory exists, creating it if necessary
pub fn ensure_directory_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path).map_err(GitError::Io)?;
    } else if !path.is_dir() {
        return Err(GitError::Path(path.to_path_buf()));
    }
    
    Ok(())
}