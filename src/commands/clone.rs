use std::path::{Path, PathBuf};
use std::fs;
use tokio::runtime::Runtime;

use crate::core::{GitError, Result};
// use crate::repository::Repository; // gix handles repository creation
// use crate::transport::TorConnection; // gix handles transport via registration
use gix::clone; // For prepare_clone
use gix::create; // For repository creation options
use gix::progress; // For progress reporting
use gix::credentials; // For potential authentication
use gix::interrupt; // For cancellation support

/// Implements the `clone` command functionality
pub struct CloneCommand {
    /// Remote repository URL
    url: String,
    /// Local destination path
    target: PathBuf,
    /// Optional clone depth
    depth: Option<usize>,
    /// Whether to clone anonymously over Tor
    anonymous: bool,
}

impl CloneCommand {
    /// Create a new clone command
    pub fn new(url: &str, target: &Path, depth: Option<usize>, anonymous: bool) -> Self {
        Self {
            url: url.to_string(),
            target: target.to_path_buf(),
            depth,
            anonymous,
        }
    }
    
    /// Execute the clone command using gitoxide
    pub fn execute(&self) -> Result<()> {
        println!("Cloning {} into {}", self.url, self.target.display());

        // 1. Prepare Clone Operation
        // Transport registration should have happened in main.rs
        // gix will automatically create the target directory.
        // It also handles the check for non-empty existing directories.
        let create_opts = create::Options {
            // Use defaults: create::Kind::WithWorktree, create::hash_kind::SHA1
            ..Default::default()
        };
        let mut prepare_fetch = match clone::prepare_clone(self.url.clone(), &self.target, create_opts) {
            Ok(prepare) => prepare,
            Err(clone::Error::PrepareClone(gix::config::path::interpolate::Error::Io(err)))
                if err.kind() == std::io::ErrorKind::AlreadyExists => {
                // Improve error message for existing, non-empty directory
                return Err(GitError::Repository(format!(
                    "Destination path '{}' already exists and is not an empty directory.",
                    self.target.display()
                )));
            }
            Err(e) => {
                return Err(GitError::Repository(format!("Failed to prepare clone: {}", e), Some(self.target.clone())));
            }
        };

        // 2. Configure Fetch
        // TODO: Implement proper progress reporting
        let mut progress = progress::Discard;
        // TODO: Configure credentials if needed
        // let mut creds = credentials::helper::Helper::new(...);
        // prepare_fetch = prepare_fetch.with_credentials(&mut creds);

        // Handle depth if specified (shallow clone)
        if let Some(depth) = self.depth {
            prepare_fetch = prepare_fetch.with_shallow(clone::Shallow::Depth(depth.try_into().unwrap_or(1)));
            println!("Performing shallow clone with depth {}", depth);
        }

        // 3. Execute Fetch and Checkout
        println!("Fetching objects and checking out...");
        let (mut repo, fetch_outcome) = prepare_fetch
            .fetch_then_checkout(&mut progress, &interrupt::IS_INTERRUPTED)
            .map_err(|e| GitError::Transport(format!("Clone fetch/checkout failed: {}", e), Some(self.url.clone())))?;
        
        println!("Fetch outcome: {} refs updated.", fetch_outcome.ref_map.mappings.len());
        // log::debug!("Fetch outcome details: {:?}", fetch_outcome); // Optional

        // 4. Configure the cloned repository (e.g., set up remote 'origin')
        // gix::clone usually sets up 'origin' automatically based on the source URL.
        // We can verify or add more config if needed.
        {
            let mut config = repo.config_snapshot_mut();
            // Example: Ensure remote origin URL is set correctly
            config.set_raw_value("remote.origin.url", &self.url)?;
            config.set_raw_value("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*")?;
            // Add more config as needed (e.g., user name/email if not global)
        } // config snapshot is dropped, changes are saved (if possible)

        println!("Clone completed successfully into {}", self.target.display());
        Ok(())
    }
}