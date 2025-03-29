use std::path::{Path, PathBuf};
use std::fs;
use std::collections::HashMap;

use crate::core::{GitError, Result, ObjectId};

/// Storage for Git references
pub struct RefStorage {
    path: PathBuf,
    refs: HashMap<String, String>,
}

impl RefStorage {
    /// Create a new reference storage
    pub fn new(repo_path: &Path) -> Self {
        Self {
            path: repo_path.to_path_buf(),
            refs: HashMap::new(),
        }
    }
    
    /// Get a reference value
    pub fn get_ref(&self, name: &str) -> Result<Option<String>> {
        let ref_path = self.path.join(name);
        
        if ref_path.exists() {
            let content = fs::read_to_string(&ref_path)
                .map_err(GitError::Io)?
                .trim()
                .to_string();
                
            return Ok(Some(content));
        }
        
        // Check packed-refs
        let packed_refs_path = self.path.join("packed-refs");
        if packed_refs_path.exists() {
            let content = fs::read_to_string(&packed_refs_path)
                .map_err(GitError::Io)?;
                
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() == 2 && parts[1] == name {
                    return Ok(Some(parts[0].to_string()));
                }
            }
        }
        
        Ok(None)
    }
    
    /// Set a reference value
    pub fn update_ref(&mut self, name: &str, value: &str) -> Result<()> {
        let ref_path = self.path.join(name);
        
        // Ensure the directory exists
        if let Some(parent) = ref_path.parent() {
            fs::create_dir_all(parent).map_err(GitError::Io)?;
        }
        
        fs::write(&ref_path, format!("{}\n", value))
            .map_err(GitError::Io)?;
            
        self.refs.insert(name.to_string(), value.to_string());
        
        Ok(())
    }
    
    /// Delete a reference
    pub fn delete_ref(&mut self, name: &str) -> Result<()> {
        let ref_path = self.path.join(name);
        
        if ref_path.exists() {
            fs::remove_file(&ref_path).map_err(GitError::Io)?;
            self.refs.remove(name);
        }
        
        Ok(())
    }
    
    /// Get all references
    pub fn list_refs(&self, prefix: &str) -> Result<Vec<String>> {
        let mut refs = Vec::new();
        let ref_dir = self.path.join(prefix);
        
        if ref_dir.exists() {
            Self::list_refs_recursive(&ref_dir, &self.path, &mut refs)?;
        }
        
        Ok(refs)
    }
    
    /// Recursively list references
    fn list_refs_recursive(dir: &Path, base: &Path, result: &mut Vec<String>) -> Result<()> {
        for entry in fs::read_dir(dir).map_err(GitError::Io)? {
            let entry = entry.map_err(GitError::Io)?;
            let path = entry.path();
            
            if path.is_dir() {
                Self::list_refs_recursive(&path, base, result)?;
            } else {
                if let Ok(relative) = path.strip_prefix(base) {
                    result.push(relative.to_string_lossy().into_owned());
                }
            }
        }
        
        Ok(())
    }
    
    /// Get the HEAD reference
    pub fn head(&self) -> Result<Option<String>> {
        self.get_ref("HEAD")
    }
    
    /// Update the HEAD reference
    pub fn set_head(&mut self, target: &str) -> Result<()> {
        self.update_ref("HEAD", target)
    }
}