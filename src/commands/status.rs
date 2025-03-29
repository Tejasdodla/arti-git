use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::fs;
use std::io;

use crate::core::{GitError, Result, ObjectId};
use crate::repository::Repository;

/// File status in the working directory
#[derive(Debug, PartialEq, Eq)]
pub enum FileStatus {
    /// New file
    New,
    /// Modified file
    Modified,
    /// Deleted file
    Deleted,
    /// Renamed file
    Renamed(String),
}

/// Implements the `status` command functionality
pub struct StatusCommand {
    /// Path to the repository
    path: PathBuf,
    /// Whether to show short status
    short: bool,
}

impl StatusCommand {
    /// Create a new status command
    pub fn new(path: &Path, short: bool) -> Self {
        Self {
            path: path.to_path_buf(),
            short,
        }
    }
    
    /// Execute the status command
    pub fn execute(&self) -> Result<()> {
        // Open the repository
        let repo = Repository::open(&self.path)?;
        
        // Get the current branch
        let refs_storage = repo.get_refs_storage();
        let head_ref = refs_storage.head()?;
        
        match head_ref {
            Some(ref_name) => {
                if ref_name.starts_with("refs/heads/") {
                    let branch_name = &ref_name["refs/heads/".len()..];
                    println!("On branch {}", branch_name);
                } else {
                    println!("HEAD detached at {}", ref_name);
                }
            },
            None => {
                println!("No commits yet");
            }
        }
        
        // Get the working directory status
        let (index_changes, working_dir_changes) = self.get_status(&repo)?;
        
        if index_changes.is_empty() && working_dir_changes.is_empty() {
            println!("Nothing to commit, working tree clean");
            return Ok(());
        }
        
        // Show changes staged for commit
        if !index_changes.is_empty() {
            println!("\nChanges to be committed:");
            println!("  (use \"git reset HEAD <file>...\" to unstage)");
            
            for (path, status) in &index_changes {
                match status {
                    FileStatus::New => println!("        new file:   {}", path),
                    FileStatus::Modified => println!("        modified:   {}", path),
                    FileStatus::Deleted => println!("        deleted:    {}", path),
                    FileStatus::Renamed(old) => println!("        renamed:    {} -> {}", old, path),
                }
            }
        }
        
        // Show changes not staged for commit
        if !working_dir_changes.is_empty() {
            println!("\nChanges not staged for commit:");
            println!("  (use \"git add <file>...\" to update what will be committed)");
            println!("  (use \"git checkout -- <file>...\" to discard changes in working directory)");
            
            for (path, status) in &working_dir_changes {
                match status {
                    FileStatus::New => println!("        new file:   {}", path),
                    FileStatus::Modified => println!("        modified:   {}", path),
                    FileStatus::Deleted => println!("        deleted:    {}", path),
                    FileStatus::Renamed(old) => println!("        renamed:    {} -> {}", old, path),
                }
            }
        }
        
        Ok(())
    }
    
    /// Get the status of the repository
    fn get_status(&self, repo: &Repository) -> Result<(HashMap<String, FileStatus>, HashMap<String, FileStatus>)> {
        let index_storage = repo.get_index_storage();
        let object_storage = repo.get_object_storage();
        let working_dir = repo.get_working_dir()?;
        
        let mut index_changes = HashMap::new();
        let mut working_dir_changes = HashMap::new();
        
        // Get the current HEAD commit
        let refs_storage = repo.get_refs_storage();
        let head_commit_id = match refs_storage.resolve_reference("HEAD")? {
            Some(commit_id) => commit_id,
            None => {
                // No HEAD commit yet (new repo), compare everything with empty tree
                let index_entries = index_storage.get_all_entries()?;
                
                // All files in the index are considered new
                for (path, _) in index_entries {
                    index_changes.insert(path.clone(), FileStatus::New);
                }
                
                // Scan working directory for untracked files
                self.scan_working_dir(
                    &working_dir,
                    &index_entries,
                    &mut working_dir_changes,
                )?;
                
                return Ok((index_changes, working_dir_changes));
            }
        };
        
        // Get the tree from the HEAD commit
        let head_commit = object_storage.read_commit(&head_commit_id)?;
        let head_tree_id = head_commit.tree();
        let head_tree = object_storage.read_tree(&head_tree_id)?;
        let head_entries = head_tree.get_entries_map();
        
        // Get the index entries
        let index_entries = index_storage.get_all_entries()?;
        
        // Compare HEAD with index
        for (path, head_entry) in &head_entries {
            match index_entries.get(path) {
                Some(index_entry) => {
                    // File exists in both HEAD and index
                    if head_entry.object_id() != index_entry.object_id() {
                        // Content has changed
                        index_changes.insert(path.clone(), FileStatus::Modified);
                    }
                },
                None => {
                    // File exists in HEAD but not in index
                    index_changes.insert(path.clone(), FileStatus::Deleted);
                }
            }
        }
        
        // Find new files in index
        for (path, index_entry) in &index_entries {
            if !head_entries.contains_key(path) {
                // File exists in index but not in HEAD
                index_changes.insert(path.clone(), FileStatus::New);
            }
        }
        
        // Compare index with working directory
        self.scan_working_dir(
            &working_dir,
            &index_entries,
            &mut working_dir_changes,
        )?;
        
        Ok((index_changes, working_dir_changes))
    }
    
    /// Scan the working directory and compare with index
    fn scan_working_dir(
        &self,
        working_dir: &Path,
        index_entries: &HashMap<String, crate::repository::IndexEntry>,
        working_dir_changes: &mut HashMap<String, FileStatus>,
    ) -> Result<()> {
        let mut visited_paths = Vec::new();
        self.scan_directory(
            working_dir,
            working_dir,
            index_entries,
            working_dir_changes,
            &mut visited_paths,
        )?;
        
        // Find deleted files (in index but not in working directory)
        for path in index_entries.keys() {
            if !visited_paths.contains(path) {
                working_dir_changes.insert(path.clone(), FileStatus::Deleted);
            }
        }
        
        Ok(())
    }
    
    /// Recursively scan a directory and compare files with index
    fn scan_directory(
        &self,
        repo_root: &Path,
        current_dir: &Path,
        index_entries: &HashMap<String, crate::repository::IndexEntry>,
        working_dir_changes: &mut HashMap<String, FileStatus>,
        visited_paths: &mut Vec<String>,
    ) -> Result<()> {
        let entries = match fs::read_dir(current_dir) {
            Ok(entries) => entries,
            Err(err) => return Err(GitError::Io(err)),
        };
        
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => return Err(GitError::Io(err)),
            };
            
            let path = entry.path();
            
            // Skip .git directory
            if path.file_name().map(|n| n == ".git").unwrap_or(false) {
                continue;
            }
            
            // Get the relative path from the repo root
            let relative_path = path.strip_prefix(repo_root)
                .map_err(|_| GitError::InvalidPath(path.clone()))?;
            let relative_path_str = relative_path.to_str()
                .ok_or_else(|| GitError::InvalidPath(path.clone()))?
                .replace("\\", "/");  // Normalize path separators
            
            if path.is_dir() {
                // Recursively scan subdirectories
                self.scan_directory(
                    repo_root,
                    &path,
                    index_entries,
                    working_dir_changes,
                    visited_paths,
                )?;
            } else {
                visited_paths.push(relative_path_str.clone());
                
                match index_entries.get(&relative_path_str) {
                    Some(index_entry) => {
                        // File exists in both index and working dir
                        // Compare content
                        let file_content = fs::read(&path)
                            .map_err(|e| GitError::Io(e))?;
                        
                        let object_id = repo_root.join(".git/objects")
                            .exists()
                            .then(|| {
                                // Hash the content to get object ID
                                crate::core::hash_object(&file_content, "blob")
                            })
                            .flatten();
                        
                        if let Some(object_id) = object_id {
                            if object_id != *index_entry.object_id() {
                                // Content has changed
                                working_dir_changes.insert(relative_path_str, FileStatus::Modified);
                            }
                        }
                    },
                    None => {
                        // File exists in working dir but not in index
                        // (untracked file)
                        working_dir_changes.insert(relative_path_str, FileStatus::New);
                    }
                }
            }
        }
        
        Ok(())
    }
}