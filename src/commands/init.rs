use std::path::{Path, PathBuf};

use crate::core::{GitError, Result};
use crate::repository::{Repository, InitOptions};

/// Implements the `init` command functionality
pub struct InitCommand {
    /// Path where to initialize the repository
    path: PathBuf,
    /// Whether to create a bare repository
    bare: bool,
    /// Initial branch name
    initial_branch: String,
    /// Whether to initialize with a .gitignore file
    init_gitignore: bool,
}

impl InitCommand {
    /// Create a new init command
    pub fn new(path: &Path, bare: bool, initial_branch: Option<&str>, init_gitignore: bool) -> Self {
        Self {
            path: path.to_path_buf(),
            bare,
            initial_branch: initial_branch
                .map(|s| s.to_string())
                .unwrap_or_else(|| "main".to_string()),
            init_gitignore,
        }
    }
    
    /// Execute the init command
    pub fn execute(&self) -> Result<()> {
        println!("Initializing git repository in {}", self.path.display());
        
        // Create options
        let options = InitOptions {
            bare: self.bare,
            initial_branch: self.initial_branch.clone(),
            init_gitignore: self.init_gitignore,
        };
        
        // Initialize the repository
        let repo = Repository::init_with_options(&self.path, &options)?;
        
        if self.bare {
            println!("Initialized empty bare repository in {}", self.path.display());
        } else {
            println!("Initialized empty repository in {}", self.path.join(".git").display());
            if self.init_gitignore {
                println!("Created default .gitignore file");
            }
        }
        
        println!("Initial branch: {}", self.initial_branch);
        
        Ok(())
    }
}