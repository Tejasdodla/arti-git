mod config;
mod refs;
mod storage;

use std::path::{Path, PathBuf};
use std::fs;

use crate::core::{GitError, Result, ObjectId, ObjectType, ObjectStorage};

use self::config::Config;
use self::refs::RefStorage;
use self::storage::FileSystemObjectStore;

/// Represents a Git repository
pub struct Repository {
    path: PathBuf,
    object_store: Box<dyn ObjectStorage>,
    refs: RefStorage,
    config: Config,
    bare: bool,
}

impl Repository {
    /// Initialize a new Git repository
    pub fn init(path: &Path, bare: bool) -> Result<Self> {
        // Create repository structure
        let repo_path = if bare {
            path.to_path_buf()
        } else {
            path.join(".git")
        };
        
        // Create directories
        fs::create_dir_all(&repo_path)?;
        fs::create_dir_all(repo_path.join("objects"))?;
        fs::create_dir_all(repo_path.join("objects/pack"))?;
        fs::create_dir_all(repo_path.join("objects/info"))?;
        fs::create_dir_all(repo_path.join("refs/heads"))?;
        fs::create_dir_all(repo_path.join("refs/tags"))?;
        
        // Create initial HEAD
        fs::write(repo_path.join("HEAD"), b"ref: refs/heads/main\n")?;
        
        // Create empty config
        let config = Config::new();
        config.save_to_file(&repo_path.join("config"))?;
        
        // Initialize and return repository
        let object_store = Box::new(FileSystemObjectStore::new(repo_path.join("objects")));
        let refs = RefStorage::new(&repo_path);
        
        Ok(Self {
            path: repo_path,
            object_store,
            refs,
            config,
            bare,
        })
    }
    
    /// Open an existing Git repository
    pub fn open(path: &Path) -> Result<Self> {
        // Determine if this is a repository or a path to a repository
        let (repo_path, is_bare) = if path.join(".git").is_dir() {
            (path.join(".git"), false)
        } else if path.join("objects").is_dir() && path.join("refs").is_dir() {
            (path.to_path_buf(), true)
        } else {
            return Err(GitError::Path(path.to_path_buf()));
        };
        
        // Initialize components
        let object_store = Box::new(FileSystemObjectStore::new(repo_path.join("objects")));
        let refs = RefStorage::new(&repo_path);
        let config = Config::load_from_file(&repo_path.join("config"))?;
        
        Ok(Self {
            path: repo_path,
            object_store,
            refs,
            config,
            bare: is_bare,
        })
    }
    
    /// Get the path of the repository
    pub fn path(&self) -> &Path {
        &self.path
    }
    
    /// Check if the repository is bare
    pub fn is_bare(&self) -> bool {
        self.bare
    }
    
    /// Get the working directory path
    pub fn workdir(&self) -> Option<PathBuf> {
        if self.bare {
            None
        } else {
            Some(self.path.parent().unwrap().to_path_buf())
        }
    }
    
    // TODO: Implement repository operations such as add, commit, etc.
}