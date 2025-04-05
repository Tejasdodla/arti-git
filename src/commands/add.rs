use std::path::{Path, PathBuf};
use std::fs;
// use gix::index::entry::Mode; // Not needed directly
// use gix::index::entry::Flags; // Not needed directly
use gix::index::add::Options as AddOptions; // Options for adding
use gix::progress; // For progress reporting
use gix::interrupt; // For cancellation
use gix::Repository as GixRepository; // Use alias

use crate::core::{GitError, Result}; // ObjectId, ObjectType, operations not needed
// use crate::repository::Repository; // Replaced by gix::Repository

/// Implements the `add` command functionality
pub struct AddCommand {
    /// Paths to add to the index
    paths: Vec<PathBuf>,
    /// Repository path
    repo_path: PathBuf,
    /// Whether to add all files
    all: bool,
}

impl AddCommand {
    /// Create a new add command
    pub fn new(paths: Vec<PathBuf>, repo_path: &Path, all: bool) -> Self {
        Self {
            paths,
            repo_path: repo_path.to_path_buf(),
            all,
        }
    }
    
    /// Execute the add command
    pub fn execute(&self) -> Result<()> {
        // Open the gitoxide repository instance
        let repo = GixRepository::open(&self.repo_path)
            .map_err(|e| GitError::Repository(format!("Failed to open gitoxide repository: {}", e), Some(self.repo_path.clone())))?;

        // Get mutable access to the index file
        let mut index = repo.index_mut()?;

        // Define add options
        let options = AddOptions::default(); // Use default options for now
        let mut progress = progress::Discard; // TODO: Implement progress

        if self.all {
            println!("Adding all changes to the index...");
            // Use add_all to stage everything (new, modified, deleted)
            index.add_all(&repo, &interrupt::IS_INTERRUPTED, &mut progress, options)
                .map_err(|e| GitError::Repository(format!("Failed to add all changes: {}", e), Some(self.repo_path.clone())))?;
            println!("Staged all changes.");
        } else if self.paths.is_empty() {
            return Err(GitError::InvalidArgument("No paths specified to add.".to_string()));
        } else {
            println!("Adding specified paths to the index...");
            // Add specific paths
            // Note: add_by_path expects paths relative to the workdir root.
            // We assume the input paths are already relative or handle absolute paths if needed.
            let results = index.add_by_path(&self.paths, options, &repo.objects)?;
            // TODO: Check results for potential errors per path?
            // For now, assume success if no error was returned overall.
            println!("Staged {} paths.", self.paths.len());
        }

        // Write the updated index back to disk
        index.write(gix::index::write::Options::default())
            .map_err(|e| GitError::Repository(format!("Failed to write index: {}", e), Some(self.repo_path.clone())))?;

        println!("Changes staged successfully.");
        Ok(())
    }
}

// Helper functions add_file_to_index and remove_file_from_index are no longer needed.
}