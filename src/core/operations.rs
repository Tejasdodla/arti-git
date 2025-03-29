use std::path::{Path, PathBuf};
use std::collections::HashSet;

use gix::{Repository, oid};
use gix_hash::ObjectId;
use gix_revision::spec::parse;

use crate::core::{GitError, Result};

/// Represents a file status in the repository
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    /// File is new and untracked
    Untracked,
    /// File is new and staged
    New,
    /// File is modified but not staged
    Modified,
    /// File is modified and staged
    Staged,
    /// File is deleted from filesystem but still in index
    Deleted,
    /// File is deleted from index (staged for deletion)
    DeletedStaged,
    /// File has merge conflicts
    Conflicted,
}

/// Represents a change to a file in the repository
#[derive(Debug, Clone)]
pub struct FileChange {
    /// Path of the file
    pub path: PathBuf,
    /// Status of the file
    pub status: FileStatus,
    /// Optional path in case of rename
    pub original_path: Option<PathBuf>,
}

/// Get the status of the repository
pub fn status(repo: &Repository) -> Result<Vec<FileChange>> {
    let mut changes = Vec::new();
    
    // Get repository paths
    let workdir = repo.work_dir()
        .map_err(|e| GitError::Repository(format!("Failed to get work directory: {}", e)))?;
    
    // Get current index
    let index = repo.index()
        .map_err(|e| GitError::Repository(format!("Failed to get repository index: {}", e)))?;
    
    // Get HEAD commit
    let head_commit = match repo.head_commit() {
        Ok(commit) => Some(commit),
        Err(_) => None, // Repository might be empty
    };
    
    // Check for indexed changes (staged)
    if let Some(head) = &head_commit {
        let head_tree = head.tree()
            .map_err(|e| GitError::Repository(format!("Failed to get HEAD tree: {}", e)))?;
            
        // Compare index to HEAD
        let diff = gix_diff::tree::Changes::new(Some(&head_tree), Some(&index))
            .map_err(|e| GitError::Repository(format!("Failed to diff index against HEAD: {}", e)))?;
        
        // Process staged changes
        for change in diff.iter() {
            let change = change.map_err(|e| GitError::Repository(format!("Error iterating diff: {}", e)))?;
            
            let path = match &change.location {
                gix_diff::tree::Location::Addition { path, .. } => path,
                gix_diff::tree::Location::Deletion { path, .. } => path,
                gix_diff::tree::Location::Modification { path, .. } => path,
            };
            
            let abs_path = workdir.join(path);
            
            // Determine file status
            let status = match change.change {
                gix_diff::tree::Change::Addition { .. } => FileStatus::New,
                gix_diff::tree::Change::Deletion { .. } => FileStatus::DeletedStaged,
                gix_diff::tree::Change::Modification { .. } => FileStatus::Staged,
            };
            
            changes.push(FileChange {
                path: abs_path,
                status,
                original_path: None,
            });
        }
    }
    
    // Check for unstaged changes (modified)
    let mut unstaged = HashSet::new();
    
    // Compare working directory to index
    let diff_options = gix_diff::diff_tree_to_workdir_with_index(repo)
        .map_err(|e| GitError::Repository(format!("Failed to diff workdir: {}", e)))?;
        
    for delta in diff_options.deltas() {
        match delta.status() {
            gix_diff::Status::Untracked => {
                let path = PathBuf::from(delta.path().expect("Delta must have a path"));
                let abs_path = workdir.join(&path);
                
                unstaged.insert(path.to_string_lossy().to_string());
                
                changes.push(FileChange {
                    path: abs_path,
                    status: FileStatus::Untracked,
                    original_path: None,
                });
            },
            gix_diff::Status::Modified => {
                let path = PathBuf::from(delta.path().expect("Delta must have a path"));
                let abs_path = workdir.join(&path);
                
                unstaged.insert(path.to_string_lossy().to_string());
                
                // Only add if not already tracked as staged
                if !changes.iter().any(|c| c.path == abs_path) {
                    changes.push(FileChange {
                        path: abs_path,
                        status: FileStatus::Modified,
                        original_path: None,
                    });
                }
            },
            gix_diff::Status::Deleted => {
                let path = PathBuf::from(delta.path().expect("Delta must have a path"));
                let abs_path = workdir.join(&path);
                
                unstaged.insert(path.to_string_lossy().to_string());
                
                changes.push(FileChange {
                    path: abs_path,
                    status: FileStatus::Deleted,
                    original_path: None,
                });
            },
            gix_diff::Status::Renamed => {
                let old_path = PathBuf::from(delta.old_path().expect("Delta must have an old path"));
                let new_path = PathBuf::from(delta.path().expect("Delta must have a path"));
                
                let abs_old_path = workdir.join(&old_path);
                let abs_new_path = workdir.join(&new_path);
                
                unstaged.insert(new_path.to_string_lossy().to_string());
                
                changes.push(FileChange {
                    path: abs_new_path,
                    status: FileStatus::Modified,
                    original_path: Some(abs_old_path),
                });
            },
            gix_diff::Status::Conflicted => {
                let path = PathBuf::from(delta.path().expect("Delta must have a path"));
                let abs_path = workdir.join(&path);
                
                unstaged.insert(path.to_string_lossy().to_string());
                
                changes.push(FileChange {
                    path: abs_path,
                    status: FileStatus::Conflicted,
                    original_path: None,
                });
            },
            _ => {
                // Other statuses like Ignored are not included in our status report
            }
        }
    }
    
    Ok(changes)
}

/// Create a new branch in the repository
pub fn create_branch(repo: &Repository, name: &str, start_point: Option<&str>) -> Result<ObjectId> {
    // Get the starting point commit
    let commit_id = match start_point {
        Some(rev) => {
            // Parse the revision
            let revision = parse(rev)
                .map_err(|e| GitError::InvalidArgument(format!("Invalid revision '{}': {}", rev, e)))?;
                
            // Resolve the revision to a commit
            repo.rev_resolve(&revision)
                .map_err(|e| GitError::Repository(format!("Failed to resolve '{}': {}", rev, e)))?
                .attach(repo)
                .object()
                .map_err(|e| GitError::Repository(format!("Failed to get object: {}", e)))?
                .into_commit()
                .map_err(|e| GitError::Repository(format!("'{}' is not a commit: {}", rev, e)))?
                .id
        },
        None => {
            // Use HEAD as the starting point
            repo.head_commit()
                .map_err(|e| GitError::Repository(format!("Failed to get HEAD commit: {}", e)))?
                .id
        }
    };
    
    // Create the reference
    let ref_name = format!("refs/heads/{}", name);
    repo.references.create(&ref_name, commit_id, false, &format!("create branch {}", name))
        .map_err(|e| GitError::Repository(format!("Failed to create branch '{}': {}", name, e)))?;
    
    Ok(commit_id)
}

/// List all branches in the repository
pub fn list_branches(repo: &Repository, show_remote: bool) -> Result<Vec<String>> {
    let mut branches = Vec::new();
    
    // Get all references
    let refs = repo.references()
        .map_err(|e| GitError::Repository(format!("Failed to get references: {}", e)))?;
    
    let refs_list = refs.all()
        .map_err(|e| GitError::Repository(format!("Failed to list references: {}", e)))?;
    
    // Filter and format branch names
    for reference in refs_list {
        let reference = reference
            .map_err(|e| GitError::Repository(format!("Failed to get reference: {}", e)))?;
        
        let full_name = reference.name().as_bstr().to_string();
        
        // Handle local branches
        if full_name.starts_with("refs/heads/") {
            let branch_name = full_name.strip_prefix("refs/heads/").unwrap_or(&full_name);
            branches.push(branch_name.to_string());
        } 
        // Handle remote branches if requested
        else if show_remote && full_name.starts_with("refs/remotes/") {
            let branch_name = full_name.strip_prefix("refs/remotes/").unwrap_or(&full_name);
            branches.push(format!("remotes/{}", branch_name));
        }
    }
    
    Ok(branches)
}

/// Delete a branch from the repository
pub fn delete_branch(repo: &Repository, name: &str, force: bool) -> Result<()> {
    let ref_name = format!("refs/heads/{}", name);
    
    // Check if this is the current branch before deleting
    let head_ref = repo.head_ref()
        .map_err(|e| GitError::Repository(format!("Failed to get HEAD reference: {}", e)))?;
    
    if head_ref.name().as_bstr() == ref_name {
        return Err(GitError::Repository(format!("Cannot delete the current branch '{}'", name)));
    }
    
    // Get the reference to check if it exists and is fully merged (if not forcing)
    let branch_ref = repo.references.find(&ref_name)
        .map_err(|e| GitError::Repository(format!("Branch '{}' not found: {}", name, e)))?;
    
    if !force {
        // Check if the branch is merged into HEAD
        let head_commit = repo.head_commit()
            .map_err(|e| GitError::Repository(format!("Failed to get HEAD commit: {}", e)))?;
        
        let branch_commit = branch_ref.target_id()
            .map_err(|e| GitError::Repository(format!("Failed to get branch target: {}", e)))?;
        
        // Check if the branch commit is an ancestor of HEAD
        let is_ancestor = repo.is_ancestor_of(branch_commit, head_commit.id)
            .map_err(|e| GitError::Repository(format!("Failed to check ancestry: {}", e)))?;
        
        if !is_ancestor {
            return Err(GitError::Repository(format!(
                "Branch '{}' is not fully merged. Use force=true to delete anyway.", name
            )));
        }
    }
    
    // Delete the branch
    repo.references.delete(&ref_name)
        .map_err(|e| GitError::Repository(format!("Failed to delete branch '{}': {}", name, e)))?;
    
    Ok(())
}

/// Checkout a branch, tag, or commit
pub fn checkout(repo: &Repository, target: &str, create: bool) -> Result<ObjectId> {
    if create {
        // Create and checkout a new branch
        let head_commit = repo.head_commit()
            .map_err(|e| GitError::Repository(format!("Failed to get HEAD commit: {}", e)))?;
        
        // Create branch
        create_branch(repo, target, None)?;
        
        // Set HEAD to the new branch
        repo.references.set_head(&format!("refs/heads/{}", target))
            .map_err(|e| GitError::Repository(format!("Failed to set HEAD: {}", e)))?;
        
        return Ok(head_commit.id);
    }
    
    // Try to resolve as a branch first
    let ref_name = format!("refs/heads/{}", target);
    let target_id = match repo.references.find(&ref_name) {
        Ok(reference) => {
            // It's a branch, set HEAD to it
            repo.references.set_head(&ref_name)
                .map_err(|e| GitError::Repository(format!("Failed to set HEAD: {}", e)))?;
            
            reference.target_id()
                .map_err(|e| GitError::Repository(format!("Failed to get reference target: {}", e)))?
        },
        Err(_) => {
            // Not a branch, try as a revision
            let revision = parse(target)
                .map_err(|e| GitError::InvalidArgument(format!("Invalid revision '{}': {}", target, e)))?;
                
            let resolved = repo.rev_resolve(&revision)
                .map_err(|e| GitError::Repository(format!("Failed to resolve '{}': {}", target, e)))?;
                
            let object = resolved.attach(repo).object()
                .map_err(|e| GitError::Repository(format!("Failed to get object: {}", e)))?;
                
            let commit_id = object.into_commit()
                .map_err(|e| GitError::Repository(format!("'{}' is not a commit: {}", target, e)))?
                .id;
            
            // Set HEAD to the commit (detached)
            repo.references.set_head_detached(commit_id)
                .map_err(|e| GitError::Repository(format!("Failed to set detached HEAD: {}", e)))?;
            
            commit_id
        }
    };
    
    // Reset the working directory to match the new HEAD
    // In a real implementation, we would use a soft/hard reset based on parameters
    // and properly handle conflicts, preserving unstaged changes, etc.
    // For simplicity, we're just doing a hard reset here
    
    // TODO: Implement proper checkout with working directory update
    // This would require more complex logic to update the working directory
    
    Ok(target_id)
}

/// Show a commit log
pub fn log(repo: &Repository, limit: Option<usize>) -> Result<Vec<gix::Commit<'_>>> {
    // Get the HEAD commit
    let head = repo.head_commit()
        .map_err(|e| GitError::Repository(format!("Failed to get HEAD commit: {}", e)))?;
    
    // Create a revwalk to traverse the commit history
    let mut revwalk = repo.revwalk()
        .map_err(|e| GitError::Repository(format!("Failed to create revwalk: {}", e)))?;
        
    // Push HEAD as the starting point
    revwalk.push(head.id)
        .map_err(|e| GitError::Repository(format!("Failed to push HEAD to revwalk: {}", e)))?;
    
    // Collect commits
    let mut commits = Vec::new();
    let mut count = 0;
    let max_count = limit.unwrap_or(std::usize::MAX);
    
    for commit_id in revwalk {
        let commit_id = commit_id
            .map_err(|e| GitError::Repository(format!("Failed to get next commit: {}", e)))?;
            
        let commit = repo.find_commit(commit_id)
            .map_err(|e| GitError::Repository(format!("Failed to find commit {}: {}", commit_id, e)))?;
            
        commits.push(commit);
        
        count += 1;
        if count >= max_count {
            break;
        }
    }
    
    Ok(commits)
}

/// Format a commit object for display
pub fn format_commit(commit: &gix::Commit<'_>) -> Result<String> {
    let id = commit.id.to_hex().to_string();
    let author = commit.author().name.to_string();
    let date = commit.author().time.format_approx();
    let message = commit.message().unwrap_or_default().title().unwrap_or_default().to_string();
    
    Ok(format!("{} {} ({}) {}", id[0..7].to_string(), message, author, date))
}