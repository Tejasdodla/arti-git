use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use crate::core::{GitError, Result};
use super::{LfsClient, LfsConfig, LfsPointer};

/// Represents Git LFS filter functionality
pub struct LfsFilter {
    /// The LFS client
    client: Arc<LfsClient>,
}

impl LfsFilter {
    /// Create a new LFS filter with the given client
    pub fn new(client: Arc<LfsClient>) -> Self {
        Self { client }
    }
    
    /// Clean filter: converts a file to an LFS pointer
    pub async fn clean(&self, src_path: impl AsRef<Path>, dest_path: impl AsRef<Path>) -> Result<LfsPointer> {
        let src_path = src_path.as_ref();
        let dest_path = dest_path.as_ref();
        
        // Check if file should be tracked by LFS
        // Get file size
        let metadata = std::fs::metadata(src_path)
            .map_err(|e| GitError::IO(format!("Failed to read file metadata: {}", e)))?;
            
        let size = metadata.len();
        
        if !self.client.should_track(src_path, Some(size)) {
            // If not an LFS file, just copy the file directly
            std::fs::copy(src_path, dest_path)
                .map_err(|e| GitError::IO(format!("Failed to copy file: {}", e)))?;
                
            return Err(GitError::LfsError("File not tracked by LFS".to_string()));
        }
        
        // Store the file in LFS
        let pointer = self.client.store_file(src_path).await?;
        
        // Write the pointer to the destination
        std::fs::write(dest_path, pointer.to_string())
            .map_err(|e| GitError::IO(format!("Failed to write LFS pointer: {}", e)))?;
            
        Ok(pointer)
    }
    
    /// Smudge filter: converts an LFS pointer back to its original file
    pub async fn smudge(&self, src_path: impl AsRef<Path>, dest_path: impl AsRef<Path>) -> Result<()> {
        let src_path = src_path.as_ref();
        let dest_path = dest_path.as_ref();
        
        // Read the source file
        let content = std::fs::read_to_string(src_path)
            .map_err(|e| GitError::IO(format!("Failed to read source file: {}", e)))?;
            
        // Check if it's an LFS pointer
        if !is_lfs_pointer(&content) {
            // If not a pointer, just copy the file directly
            std::fs::copy(src_path, dest_path)
                .map_err(|e| GitError::IO(format!("Failed to copy file: {}", e)))?;
                
            return Ok(());
        }
        
        // Parse the pointer
        let pointer = match LfsPointer::parse(&content) {
            Ok(p) => p,
            Err(e) => {
                // If parsing fails, just copy the file as-is
                std::fs::copy(src_path, dest_path)
                    .map_err(|e| GitError::IO(format!("Failed to copy file: {}", e)))?;
                    
                return Err(GitError::LfsError(format!("Failed to parse LFS pointer: {}", e)));
            }
        };
        
        // Get the object from storage
        self.client.get_object(&pointer, dest_path).await?;
        
        Ok(())
    }
}

/// Install Git LFS filters in the global Git config
pub fn install_filter() -> Result<()> {
    // Check if Git is available
    let git_version = Command::new("git")
        .arg("--version")
        .output()
        .map_err(|e| GitError::External(format!("Failed to run git: {}", e)))?;
        
    if !git_version.status.success() {
        return Err(GitError::External("Git command not available".to_string()));
    }
    
    // Configure Git LFS filters
    let commands = [
        // Configure the clean filter
        ["config", "--global", "filter.lfs.clean", "git-lfs clean -- %f"],
        // Configure the smudge filter
        ["config", "--global", "filter.lfs.smudge", "git-lfs smudge -- %f"],
        // Configure the required filter
        ["config", "--global", "filter.lfs.required", "true"],
        // Configure the process filter
        ["config", "--global", "filter.lfs.process", "git-lfs filter-process"],
    ];
    
    for cmd in &commands {
        let output = Command::new("git")
            .args(cmd)
            .output()
            .map_err(|e| GitError::External(format!("Failed to run git config: {}", e)))?;
            
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(GitError::External(format!("Failed to configure Git LFS: {}", error)));
        }
    }
    
    Ok(())
}

/// Check if the content appears to be an LFS pointer
fn is_lfs_pointer(content: &str) -> bool {
    // LFS pointers typically start with "version https://git-lfs.github.com/spec/"
    content.trim().starts_with("version https://git-lfs.github.com/spec/")
}