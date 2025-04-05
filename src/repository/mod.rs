mod storage;
mod refs;
mod config;
mod commit;

use std::path::{Path, PathBuf};
use std::collections::HashMap;
use chrono::{DateTime, Utc};
use gix::index::File as IndexFile; // <-- Add use statement
use crate::core::{Result, GitError, ObjectId};
use crate::crypto::SignatureProvider;

/// Repository configuration
pub struct Config {
    /// Configuration values
    values: HashMap<String, String>,
}

impl Config {
    /// Create a new configuration
    pub fn new() -> Self {
        Self {
            values: HashMap::new(),
        }
    }

    /// Get a configuration value
    pub fn get(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }

    /// Set a configuration value
    pub fn set(&mut self, key: &str, value: &str) {
        self.values.insert(key.to_string(), value.to_string());
    }

    /// Load configuration from a Git repository
    pub fn load_from_repo(_path: &Path) -> Result<Self> {
        // In a real implementation, this would read the .git/config file
        // For now, we'll return an empty configuration
        Ok(Self::new())
    }
}

/// Git signature (author/committer)
#[derive(Clone)]
pub struct Signature {
    /// Name
    name: String,
    /// Email
    email: String,
    /// Timestamp
    time: DateTime<Utc>,
}

impl Signature {
    /// Create a new signature
    pub fn new(name: &str, email: &str, time: DateTime<Utc>) -> Self {
        Self {
            name: name.to_string(),
            email: email.to_string(),
            time,
        }
    }

    /// Convert to Git format: "Name <email> timestamp timezone"
    pub fn to_string(&self) -> String {
        format!(
            "{} <{}> {} +0000",
            self.name,
            self.email,
            self.time.timestamp()
        )
    }
}

/// Git repository
pub struct Repository {
    /// Path to the repository
    path: PathBuf,
    /// Path to the .git directory
    git_dir: PathBuf,
    /// Configuration
    config: Config,
    /// The Git index file
    index: IndexFile,
}

impl Repository {
    /// Initialize a new Git repository
    pub fn init(path: &Path) -> Result<Self> {
        let git_dir = path.join(".git");
        
        // Create directories needed for a Git repository
        for dir in &["objects", "refs/heads", "refs/tags"] {
            std::fs::create_dir_all(git_dir.join(dir))
                .map_err(|e| GitError::IO(format!("Failed to create directory {}: {}", dir, e)))?;
        }
        
        // Create HEAD file pointing to refs/heads/master
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/master\n")
            .map_err(|e| GitError::IO(format!("Failed to write HEAD file: {}", e)))?;
            
        // Create an empty config
        let config = Config::new();
        
        println!("Initialized empty Git repository in {}", git_dir.display());
        
        Ok(Self {
            path: path.to_path_buf(),
            git_dir,
            config,
        })
    }

    /// Open an existing Git repository
    pub fn open(path: &Path) -> Result<Self> {
        // Find .git directory
        let git_dir = find_git_dir(path)?;
        
        // Load configuration
        let config = Config::load_from_repo(&git_dir)?;
        let index_path = git_dir.join("index");

        // Load the index file
        let index = IndexFile::at(&index_path, gix::index::decode::Options::default())
            .map_err(|e| GitError::Repository(format!("Failed to load index file '{}': {}", index_path.display(), e), Some(path.to_path_buf())))?;

        // TODO: Initialize object store, refs store etc.
        Ok(Self {
            path: path.to_path_buf(),
            git_dir,
            config,
            index,
        })
    }
    
    /// Get repository configuration
    pub fn get_config(&self) -> &Config {
        &self.config
    }

    /// Get mutable access to the index
    pub fn index_mut(&mut self) -> &mut IndexFile {
        &mut self.index
    }

    /// Write a blob object to the object database
    pub fn write_blob(&self, data: &[u8]) -> Result<ObjectId> {
        let blob = Blob { data };
        let mut odb = gix::open(&self.git_dir)
            .map_err(|e| GitError::Repository(format!("Failed to open ODB: {}", e), Some(self.path.clone())))?
            .objects;
        let gix_oid = odb.write_buf(gix::objs::Kind::Blob, data)
            .map_err(|e| GitError::ObjectStorage(format!("Failed to write blob: {}", e)))?;
        Ok(ObjectId::from_bytes(gix_oid.as_bytes())?)
    }
    
    /// Set the HEAD reference
    pub fn set_head(&self, object_id: &ObjectId) -> Result<()> {
        let head_path = self.git_dir.join("HEAD");
        std::fs::write(&head_path, object_id.to_string())
            .map_err(|e| GitError::IO(format!("Failed to write HEAD: {}", e)))?;
        Ok(())
    }
    
    /// Create a commit
    pub fn create_commit(
        &self,
        ref_name: &str,
        author: &Signature,
        committer: &Signature,
        message: &str,
        parents: &[ObjectId],
    ) -> Result<ObjectId> {
        // In a real implementation, this would:
        // 1. Create a tree from the index
        // 2. Write the commit object
        // 3. Update the reference
        
        // For now, just return a dummy object ID
        let object_id = ObjectId::from_hex("0000000000000000000000000000000000000000")?;
        Ok(object_id)
    }
    
    /// Create a signed commit
    pub fn create_commit_signed(
        &self,
        ref_name: &str,
        author: &Signature,
        committer: &Signature,
        message: &str,
        parents: &[ObjectId],
        signature_provider: &SignatureProvider,
    ) -> Result<ObjectId> {
        // In a real implementation, this would:
        // 1. Create a tree from the index
        // 2. Build the commit object content
        // 3. Sign the commit using the signature provider
        // 4. Write the signed commit object
        // 5. Update the reference
        
        println!("Creating signed commit with anonymous identity from Tor");
        
        // For now, just return a dummy object ID
        let object_id = ObjectId::from_hex("0000000000000000000000000000000000000000")?;
        Ok(object_id)
    }
}

/// Find the .git directory for a repository
fn find_git_dir(path: &Path) -> Result<PathBuf> {
    let mut current = path.to_path_buf();
    
    loop {
        let git_dir = current.join(".git");
        if git_dir.exists() && git_dir.is_dir() {
            return Ok(git_dir);
        }
        
        if !current.pop() {
            return Err(GitError::Repository(format!(
                "Not a Git repository: {}", path.display()
            )));
        }
    }
}