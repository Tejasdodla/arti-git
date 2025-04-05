use std::path::{Path, PathBuf};

use crate::core::{GitError, Result}; // ObjectId not needed directly
// use crate::repository::Repository; // Replaced by gix
// use crate::crypto::SignatureProvider; // Signing handled differently
use gix::Repository as GixRepository;
use gix::actor;
use gix::refs::transaction::{Change, LogChange, RefEdit, PreviousValue};
use gix::refs::{Target, TargetRef};
use gix::hash::ObjectId as GixObjectId;
use std::time::SystemTime;

/// Implements the `commit` command functionality with anonymous signing support
pub struct CommitCommand<'a> {
    /// Commit message
    message: String,
    /// Whether to sign the commit
    sign: bool,
    /// Optional custom onion address for signing (identity)
    onion_address: Option<&'a str>,
    /// Repository path
    path: PathBuf,
}

impl<'a> CommitCommand<'a> {
    /// Create a new commit command
    pub fn new(message: &str, sign: bool, onion_address: Option<&'a str>, path: &Path) -> Self {
        Self {
            message: message.to_string(),
            sign,
            onion_address,
            path: path.to_path_buf(),
        }
    }
    
    /// Execute the commit command
    pub fn execute(self) -> Result<GixObjectId> { // Return gix ObjectId
        // Open the gitoxide repository instance
        let repo = GixRepository::open(&self.path)
            .map_err(|e| GitError::Repository(format!("Failed to open gitoxide repository: {}", e), Some(self.path.clone())))?;

        // TODO: Handle signing options (self.sign, self.onion_address) using gix mechanisms if possible.
        if self.sign {
            log::warn!("Commit signing requested but not yet implemented with gitoxide.");
        }

        // 1. Get the index
        let index = repo.index()?;
        // Optional: Check if index has changes compared to HEAD tree?
        // if index.is_unchanged(&repo.objects)? { ... return Err("no changes added to commit") }

        // 2. Write index to a tree
        let tree_id = index.write_tree_to(&repo.objects)
            .map_err(|e| GitError::ObjectStorage(format!("Failed to write index tree: {}", e)))?;
        println!("Written tree {}", tree_id);

        // 3. Get Author and Committer Signatures from config
        let config = repo.config_snapshot();
        let author = match config.actor() {
            Ok(actor) => actor,
            Err(e) => {
                // Try getting name/email individually or use defaults
                log::warn!("Failed to get default actor from config ({}). Trying individual values or defaults.", e);
                let name = config.string("user.name").unwrap_or_else(|| "Arti Git User".into());
                let email = config.string("user.email").unwrap_or_else(|| "arti-git@example.com".into());
                // TODO: Get timezone offset correctly
                let time = actor::Time::new(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs() as u64, 0);
                actor::Signature { name, email, time }
            }
        };
        // Use author as committer for simplicity, gitoxide might default committer too
        let committer = author.clone();

        // 4. Determine Parent Commit(s)
        let head_ref = match repo.head_ref() {
            Ok(Some(r)) => Some(r),
            Ok(None) => None, // Detached HEAD - commit is fine, just won't update a branch ref directly
            Err(e) => return Err(GitError::Repository(format!("Failed to get HEAD reference: {}", e), None)),
        };

        let parents: Vec<GixObjectId> = if let Some(TargetRef::Symbolic(target)) = head_ref.as_ref().map(|hr| hr.inner.target.as_ref()) {
            // HEAD points to a branch
            match repo.find_reference(target.name.as_ref())?.peel_to_id_in_place() {
                Ok(Some(oid)) => vec![oid], // Normal case: parent is the commit the branch points to
                Ok(None) => vec![], // Branch exists but doesn't point to a commit (unborn branch)
                Err(_) => vec![], // Error peeling ref, treat as initial commit
            }
        } else if let Some(TargetRef::Peeled(oid)) = head_ref.as_ref().map(|hr| hr.inner.target.as_ref()) {
            // Detached HEAD
            vec![*oid]
        } else {
            // No HEAD ref found (e.g., empty repo)
            vec![]
        };

        // 5. Create the commit object
        let commit_id = repo.commit(
            head_ref.as_ref().and_then(|hr| hr.name().as_symbolic()), // Only update reflog if HEAD is symbolic
            &author,
            &committer,
            &self.message,
            tree_id,
            parents.clone(), // Pass the determined parents
        ).map_err(|e| GitError::Repository(format!("Failed to create commit object: {}", e), None))?;
        println!("Created commit object: {}", commit_id);

        // 6. Update HEAD reference
        if let Some(head_ref_obj) = head_ref {
            let previous_oid = parents.get(0).cloned(); // Get the first parent as the previous OID
            let edit = RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: gix::refs::log::RefLog::AndReference,
                        force_create_reflog: false,
                        message: format!("commit: {}", self.message).into(),
                    },
                    expected: match previous_oid {
                        Some(oid) => PreviousValue::MustExistAndMatch(Target::Peeled(oid)),
                        None => PreviousValue::MustNotExist, // Initial commit
                    },
                    new: Target::Peeled(commit_id),
                },
                name: head_ref_obj.name().to_owned(), // Use the actual ref name (e.g., refs/heads/main)
                deref: true, // We want to update the ref HEAD points to, not HEAD itself if symbolic
            };
            repo.edit_references(std::iter::once(edit))?;
            println!("Updated reference {}", head_ref_obj.name().as_bstr());
        } else {
            // If HEAD was detached or didn't exist, we don't update a ref automatically.
            // Git usually updates HEAD directly in this case.
            // TODO: Check if gix repo.commit() handles detached HEAD update or if we need to do it manually.
            println!("HEAD was detached or unborn; commit created but no branch updated.");
        }

        Ok(commit_id)
    }
}