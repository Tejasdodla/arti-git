use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::fs::File;
use std::io::Read;

use tokio::fs as tokio_fs;
use tokio::io::AsyncWriteExt;
use sha2::{Sha256, Digest};

use crate::core::{GitError, Result};
use super::{LfsClient, LfsPointer, LfsStorage, LfsConfig, LfsObjectId};

/// LFS filter for Git
pub struct LfsFilter {
    /// LFS client for filter operations
    client: Arc<LfsClient>,
    
    /// LFS Storage
    storage: Arc<LfsStorage>,
}

impl LfsFilter {
    /// Create a new LFS filter
    pub fn new(client: Arc<LfsClient>, storage: Arc<LfsStorage>) -> Self {
        Self {
            client,
            storage,
        }
    }
    
    /// Clean filter: converts a file to an LFS pointer
    pub async fn clean(&self, src_path: impl AsRef<Path>, dest_path: impl AsRef<Path>) -> Result<LfsPointer> {
        let src_path = src_path.as_ref();
        let dest_path = dest_path.as_ref();
        
        // Read the source file
        let data = tokio_fs::read(&src_path).await
            .map_err(|e| GitError::LfsError(format!("Failed to read source file: {}", e)))?;
            
        // Check if the file should be tracked by LFS based on its size
        let size = data.len() as u64;
        let track_file = size >= self.client.config().size_threshold ||
                         self.client.should_track(src_path, Some(size));
                         
        if !track_file {
            // No need to convert, just copy the file as is
            tokio_fs::copy(src_path, dest_path).await
                .map_err(|e| GitError::LfsError(format!("Failed to copy file: {}", e)))?;
                
            return Err(GitError::LfsError("File not tracked by LFS".to_string()));
        }
        
        // Calculate the SHA-256 hash of the file
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let hash = format!("{:x}", hasher.finalize());
        
        // Create the object ID and pointer
        let oid_str = format!("sha256:{}", hash);
        let id = LfsObjectId::new(&oid_str);
        let mut pointer = LfsPointer::new(&oid_str, size);
        
        // Store the object
        self.storage.store_object(&id, &data).await?;
        
        // Check if IPFS is enabled
        if self.client.config().use_ipfs {
            if let Some(cid) = self.storage.get_ipfs_cid(&id) {
                // We already have a CID for this object
                pointer.set_ipfs_cid(&cid);
            } else if self.storage.has_ipfs() {
                // Try to upload to IPFS if we have access to IPFS
                if let Ok(cid) = self.client.upload_to_ipfs(&data).await {
                    pointer.set_ipfs_cid(&cid);
                }
            }
        }
        
        // Write the pointer to the destination file
        tokio_fs::write(dest_path, pointer.to_string().as_bytes()).await
            .map_err(|e| GitError::LfsError(format!("Failed to write LFS pointer: {}", e)))?;
            
        Ok(pointer)
    }
    
    /// Smudge filter: converts an LFS pointer back to its original file
    pub async fn smudge(&self, src_path: impl AsRef<Path>, dest_path: impl AsRef<Path>) -> Result<()> {
        let src_path = src_path.as_ref();
        let dest_path = dest_path.as_ref();
        
        // Read the source file
        let content = tokio_fs::read_to_string(src_path).await
            .map_err(|e| GitError::LfsError(format!("Failed to read source file: {}", e)))?;
            
        // Check if it's an LFS pointer
        if !is_lfs_pointer(&content) {
            // If not a pointer, just copy the file directly
            tokio_fs::copy(src_path, dest_path).await
                .map_err(|e| GitError::LfsError(format!("Failed to copy non-LFS file: {}", e)))?;
                
            return Ok(());
        }
        
        // Parse the pointer
        let pointer = match LfsPointer::parse(&content) {
            Ok(p) => p,
            Err(e) => {
                // Invalid pointer, just copy the file as is
                tokio_fs::copy(src_path, dest_path).await
                    .map_err(|e| GitError::LfsError(format!("Failed to copy file with invalid pointer: {}", e)))?;
                    
                return Err(GitError::LfsError(format!("Failed to parse LFS pointer: {}", e)));
            }
        };
        
        // Try to get the object from different sources based on the pointer
        
        // First, try local storage using the LFS object ID
        let id = LfsObjectId::new(&pointer.oid);
        if self.storage.has_object(&id).await {
            let data = self.storage.get_object_bytes(&id).await?;
            tokio_fs::write(dest_path, data).await
                .map_err(|e| GitError::LfsError(format!("Failed to write object to file: {}", e)))?;
                
            return Ok(());
        }
        
        // Next, try IPFS if we have a CID in the pointer
        if let Some(cid) = &pointer.ipfs_cid {
            if let Some(ipfs_client) = self.storage.ipfs_client() {
                if let Ok(data) = ipfs_client.get_file(cid).await {
                    // Store the object in local storage for future use
                    self.storage.store_object(&id, &data).await?;
                    
                    // Write the data to the destination
                    tokio_fs::write(dest_path, data).await
                        .map_err(|e| GitError::LfsError(format!("Failed to write object from IPFS to file: {}", e)))?;
                        
                    return Ok(());
                }
            }
        }
        
        // Finally, try to fetch from the LFS server
        self.client.get_object(&pointer, dest_path).await
    }
    
    /// Process filter: handle Git LFS filter process commands
    pub async fn process(&self, input: &str) -> Result<String> {
        // Parse the filter process command
        let parts: Vec<&str> = input.trim().split(':').collect();
        if parts.len() < 2 {
            return Err(GitError::LfsError("Invalid filter process command".to_string()));
        }
        
        let command = parts[0];
        let args = parts[1];
        
        match command {
            "clean" => {
                // Create temporary files for clean operation
                let temp_src = tempfile::NamedTempFile::new()
                    .map_err(|e| GitError::LfsError(format!("Failed to create temporary file: {}", e)))?;
                let temp_dest = tempfile::NamedTempFile::new()
                    .map_err(|e| GitError::LfsError(format!("Failed to create temporary file: {}", e)))?;
                    
                // Write the content to the source temporary file
                tokio_fs::write(temp_src.path(), args).await
                    .map_err(|e| GitError::LfsError(format!("Failed to write to temporary file: {}", e)))?;
                    
                // Run the clean filter
                match self.clean(temp_src.path(), temp_dest.path()).await {
                    Ok(_) => {
                        // Read the pointer back
                        let pointer = tokio_fs::read_to_string(temp_dest.path()).await
                            .map_err(|e| GitError::LfsError(format!("Failed to read pointer: {}", e)))?;
                        Ok(pointer)
                    },
                    Err(_) => {
                        // Just return the original content
                        Ok(args.to_string())
                    }
                }
            },
            "smudge" => {
                // Create temporary files for smudge operation
                let temp_src = tempfile::NamedTempFile::new()
                    .map_err(|e| GitError::LfsError(format!("Failed to create temporary file: {}", e)))?;
                let temp_dest = tempfile::NamedTempFile::new()
                    .map_err(|e| GitError::LfsError(format!("Failed to create temporary file: {}", e)))?;
                    
                // Write the content to the source temporary file
                tokio_fs::write(temp_src.path(), args).await
                    .map_err(|e| GitError::LfsError(format!("Failed to write to temporary file: {}", e)))?;
                    
                // Run the smudge filter
                match self.smudge(temp_src.path(), temp_dest.path()).await {
                    Ok(_) => {
                        // Read the file back
                        let content = tokio_fs::read_to_string(temp_dest.path()).await
                            .map_err(|e| GitError::LfsError(format!("Failed to read smudged file: {}", e)))?;
                        Ok(content)
                    },
                    Err(_) => {
                        // Return the original content if smudge fails
                        Ok(args.to_string())
                    }
                }
            },
            _ => Err(GitError::LfsError(format!("Unsupported filter command: {}", command))),
        }
    }
}

/// Install Git LFS filters in the global Git config
pub fn install_filter() -> Result<()> {
    use std::process::Command;
    
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
        ["config", "--global", "filter.lfs.clean", "arti-git lfs clean -- %f"],
        // Configure the smudge filter
        ["config", "--global", "filter.lfs.smudge", "arti-git lfs smudge -- %f"],
        // Configure the required filter
        ["config", "--global", "filter.lfs.required", "true"],
        // Configure the process filter
        ["config", "--global", "filter.lfs.process", "arti-git lfs filter-process"],
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

/// Install Git LFS filters in a specific repo
pub fn install_filter_in_repo(repo_path: impl AsRef<Path>) -> Result<()> {
    use std::process::Command;
    
    let repo_path = repo_path.as_ref();
    
    // Check if Git is available
    let git_version = Command::new("git")
        .current_dir(repo_path)
        .arg("--version")
        .output()
        .map_err(|e| GitError::External(format!("Failed to run git: {}", e)))?;
        
    if !git_version.status.success() {
        return Err(GitError::External("Git command not available".to_string()));
    }
    
    // Configure Git LFS filters
    let commands = [
        // Configure the clean filter
        ["config", "filter.lfs.clean", "arti-git lfs clean -- %f"],
        // Configure the smudge filter
        ["config", "filter.lfs.smudge", "arti-git lfs smudge -- %f"],
        // Configure the required filter
        ["config", "filter.lfs.required", "true"],
        // Configure the process filter
        ["config", "filter.lfs.process", "arti-git lfs filter-process"],
    ];
    
    for cmd in &commands {
        let output = Command::new("git")
            .current_dir(repo_path)
            .args(cmd)
            .output()
            .map_err(|e| GitError::External(format!("Failed to run git config: {}", e)))?;
            
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(GitError::External(format!("Failed to configure Git LFS: {}", error)));
        }
    }
    
    // Create .gitattributes file if it doesn't exist
    let gitattributes_path = repo_path.join(".gitattributes");
    if !gitattributes_path.exists() {
        std::fs::write(&gitattributes_path, "*.bin filter=lfs diff=lfs merge=lfs -text\n")
            .map_err(|e| GitError::IO(format!("Failed to create .gitattributes file: {}", e)))?;
    }
    
    Ok(())
}

/// Check if the content appears to be an LFS pointer
fn is_lfs_pointer(content: &str) -> bool {
    // LFS pointers typically start with "version https://git-lfs.github.com/spec/"
    content.trim().starts_with("version https://git-lfs.github.com/spec/")
}