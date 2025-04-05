use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write}; // Add Write for stdout

use crate::core::{GitError, Result}; // ObjectId not needed directly here
// use crate::repository::Repository; // Replaced by gix::Repository
use gix::Repository as GixRepository;
use gix::status::{self, index_as_worktree_with_renames, Platform, Options};
use gix::progress; // For status progress
use gix::bstr::BString; // For path display

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
        // Open the gitoxide repository instance
        let repo = GixRepository::open(&self.path)
            .map_err(|e| GitError::Repository(format!("Failed to open gitoxide repository: {}", e), Some(self.path.clone())))?;

        // Get the current branch or detached HEAD state
        match repo.head_name()? {
            Some(head) => println!("On branch {}", head.shorten()),
            None => match repo.head_id()? {
                Some(id) => println!("HEAD detached at {}", id.shorten()?),
                None => println!("On unborn branch (no commits yet)"), // Or handle as error?
            }
        }

        // --- Get Status using gitoxide ---
        let mut progress = progress::Discard; // TODO: Implement progress
        let mut platform = repo.status(&mut progress, Options::default())?; // Use default options for now
        let entries = platform.entries();

        if entries.is_empty() {
            println!("Nothing to commit, working tree clean");
            return Ok(());
        }

        // --- Format and Print Status ---
        let mut stdout = io::stdout(); // Lock stdout for efficient writing

        // Group entries by status type for standard output format
        let mut staged_new = Vec::new();
        let mut staged_modified = Vec::new();
        let mut staged_deleted = Vec::new();
        let mut staged_renamed = Vec::new(); // TODO: Handle renames if detected
        let mut unstaged_modified = Vec::new();
        let mut unstaged_deleted = Vec::new();
        let mut untracked = Vec::new();
        let mut conflicts = Vec::new();

        for entry in entries {
            let path = entry.rela_path; // This is a BStr
            match (entry.index_status, entry.worktree_status) {
                // Staged changes
                (Some(status::index::Status::Added), _) => staged_new.push(path),
                (Some(status::index::Status::Modified), _) => staged_modified.push(path),
                (Some(status::index::Status::Deleted), _) => staged_deleted.push(path),
                // TODO: Handle staged renames/copies if status::index::Status provides them

                // Unstaged changes (only if not also staged differently)
                (None, Some(status::worktree::Status::Modified)) => unstaged_modified.push(path),
                (Some(status::index::Status::Unchanged), Some(status::worktree::Status::Modified)) => unstaged_modified.push(path),
                (Some(status::index::Status::Modified), Some(status::worktree::Status::Deleted)) => unstaged_deleted.push(path), // Deleted after staged modify
                (None, Some(status::worktree::Status::Deleted)) => unstaged_deleted.push(path), // Deleted without staging

                // Untracked
                (None, Some(status::worktree::Status::Added)) => untracked.push(path),

                // Conflicts (both staged and unstaged)
                (Some(status::index::Status::Conflict), _) | (_, Some(status::worktree::Status::Conflict)) => conflicts.push(path),

                // Other cases (e.g., unchanged, ignored) are ignored for standard status output
                _ => {}
            }
        }

        // Print Staged Changes
        if !staged_new.is_empty() || !staged_modified.is_empty() || !staged_deleted.is_empty() || !staged_renamed.is_empty() {
            writeln!(stdout, "\nChanges to be committed:")?;
            writeln!(stdout, "  (use \"arti-git reset HEAD <file>...\" to unstage)")?; // Adjust command name
            for path in staged_new { writeln!(stdout, "\tnew file:   {}", path)?; }
            for path in staged_modified { writeln!(stdout, "\tmodified:   {}", path)?; }
            for path in staged_deleted { writeln!(stdout, "\tdeleted:    {}", path)?; }
            // for (old, new) in staged_renamed { writeln!(stdout, "\trenamed:    {} -> {}", old, new)?; }
            writeln!(stdout)?;
        }

        // Print Conflicts
        if !conflicts.is_empty() {
            writeln!(stdout, "\nUnmerged paths:")?;
            writeln!(stdout, "  (use \"arti-git add <file>...\" to mark resolution)")?; // Adjust command name
            for path in conflicts { writeln!(stdout, "\tboth modified: {}", path)?; } // Simplification
            writeln!(stdout)?;
        }

        // Print Unstaged Changes
        if !unstaged_modified.is_empty() || !unstaged_deleted.is_empty() {
            writeln!(stdout, "\nChanges not staged for commit:")?;
            writeln!(stdout, "  (use \"arti-git add <file>...\" to update what will be committed)")?; // Adjust command name
            writeln!(stdout, "  (use \"arti-git checkout -- <file>...\" to discard changes in working directory)")?; // Adjust command name
            for path in unstaged_modified { writeln!(stdout, "\tmodified:   {}", path)?; }
            for path in unstaged_deleted { writeln!(stdout, "\tdeleted:    {}", path)?; }
            writeln!(stdout)?;
        }

        // Print Untracked Files
        if !untracked.is_empty() {
            writeln!(stdout, "\nUntracked files:")?;
            writeln!(stdout, "  (use \"arti-git add <file>...\" to include in what will be committed)")?; // Adjust command name
            for path in untracked { writeln!(stdout, "\t{}", path)?; }
            writeln!(stdout)?;
        }

        // Final summary message if applicable
        if staged_new.is_empty() && staged_modified.is_empty() && staged_deleted.is_empty() && staged_renamed.is_empty() && conflicts.is_empty() {
            if !unstaged_modified.is_empty() || !unstaged_deleted.is_empty() {
                 writeln!(stdout, "no changes added to commit (use \"git add\" and/or \"git commit -a\")")?;
            } else if !untracked.is_empty() {
                 writeln!(stdout, "nothing added to commit but untracked files present (use \"git add\" to track)")?;
            }
        }

        Ok(())
    }
    
    // Old manual status logic removed
}