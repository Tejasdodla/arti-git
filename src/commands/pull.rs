use std::path::{Path, PathBuf};
use tokio::runtime::Runtime;

use crate::core::{GitError, Result, ObjectId}; // Added ObjectId
use crate::repository::Repository;
// use crate::transport::TorConnection; // Old manual connection logic removed
use gix::remote; // For connect and fetch
use gix::progress; // For fetch progress reporting
use gix::credentials; // For potential authentication callbacks
use gix::refs::{FullNameRef, TargetRef}; // For ref names and targets
use gix::Repository as GixRepository; // Use alias to avoid conflict
use gix::hash::ObjectId as GixObjectId;
use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, RefEdit, PreviousValue};
use gix::interrupt;
use gix::object::Commit;
use gix::index;
use gix::object::tree::write_to;
use gix::actor;
use std::time::SystemTime;
use gix::worktree::checkout;

/// Implements the `pull` command functionality
pub struct PullCommand {
    /// Remote name
    remote: String,
    /// Refspec for pulling (e.g., "main:main")
    refspec: Option<String>,
    /// Repository path
    path: PathBuf,
    /// Whether to use anonymous mode over Tor
    anonymous: bool,
}

impl PullCommand {
    /// Create a new pull command
    pub fn new(remote: &str, refspec: Option<&str>, path: &Path, anonymous: bool) -> Self {
        Self {
            remote: remote.to_string(),
            refspec: refspec.map(|s| s.to_string()),
            path: path.to_path_buf(),
            anonymous,
        }
    }
    
    /// Execute the pull command
    pub fn execute(&self) -> Result<()> {
        // Open the repository
        let repo = Repository::open(&self.path)?;
        
        // Get the remote URL
        let config = repo.get_config();
        let remote_url = config.get(&format!("remote.{}.url", self.remote))
            .ok_or_else(|| GitError::Reference(format!("Remote '{}' not found", self.remote)))?;
        
        println!("Pulling from {} ({})", self.remote, remote_url);
        
        if self.anonymous {
            self.pull_over_tor(&repo, remote_url)
        } else {
            self.pull_over_http(&repo, remote_url)
        }
    }
    
    /// Pull over Tor network
    fn pull_over_tor(&self, repo: &Repository, remote_url: &str) -> Result<()> {
        println!("Pulling over Tor network");
        
        // Create a Tokio runtime
        let rt = Runtime::new()
            .map_err(|e| GitError::Transport(format!("Failed to create runtime: {}", e)))?;
            
        // Execute the pull operation using gitoxide fetch and checkout
        // Note: gix fetch/connect are blocking but use async internally via transport.
        // We don't need the explicit tokio runtime block here anymore unless other
        // truly async operations are added later in this function.
        // rt.block_on(async { ... });

        // Determine what to pull based on refspec
        let (src_ref_str, dst_ref_str) = self.parse_refspec()?;
        // Construct a refspec suitable for fetch (e.g., refs/heads/main:refs/remotes/origin/main)
        // We need the remote-tracking ref as the destination for fetch.
        let remote_tracking_dst = format!("refs/remotes/{}/{}", self.remote, src_ref_str.strip_prefix("refs/heads/").unwrap_or(&src_ref_str));
        let fetch_refspec = format!("{}:{}", src_ref_str, remote_tracking_dst);
        println!("Pulling refspec '{}' from remote '{}'", fetch_refspec, self.remote);

        // Open the gitoxide repository instance
        let gix_repo = GixRepository::open(&self.path)
            .map_err(|e| GitError::Repository(format!("Failed to open gitoxide repository: {}", e), Some(self.path.clone())))?;

        // --- Connect to Remote ---
        // Transport registration should have happened in main.rs
        let mut progress = progress::Discard; // TODO: Implement actual progress reporting
        let mut remote = gix_repo.find_remote(&self.remote)?
            .with_fetch_tags(remote::fetch::Tags::None) // Don't fetch tags for pull
            .connect(remote::Direction::Fetch, &mut progress)?;
        println!("Connected to remote '{}' at URL: {}", self.remote, remote.url().map(|u| u.to_string()).unwrap_or_else(|| "N/A".into()));

        // --- Prepare and Execute Fetch ---
        let ref_specs_input = [fetch_refspec.as_str()];
        let fetch_options = remote::fetch::Options::default(); // Use default options for now
        // TODO: Configure credentials callback if needed
        // let mut creds = credentials::helper::Helper::new(...);

        println!("Fetching objects...");
        let outcome = remote.fetch(&ref_specs_input, fetch_options)?;
            // .with_credentials(&mut creds)?; // Add credentials if needed

        println!("Fetch completed.");
        // outcome.ref_map contains details about updated refs
        // log::debug!("Fetch outcome: {:?}", outcome); // Optional detailed logging

        // --- Find OIDs for Merge/Checkout ---
        // Find the OID of the *local* branch we want to merge into (dst_ref_str)
        let local_ref = gix_repo.find_reference(&dst_ref_str)?;
        let local_oid = local_ref.peel_to_id_in_place()?
            .ok_or_else(|| GitError::Reference(format!("Local ref '{}' could not be resolved to an OID", dst_ref_str)))?;

        // Find the OID of the *fetched* commit using the remote-tracking ref name we used in the fetch refspec.
        println!("Looking for fetched OID at remote-tracking ref: {}", remote_tracking_dst);
        let fetched_ref = gix_repo.find_reference(&remote_tracking_dst)?;
        let fetched_oid = fetched_ref.peel_to_id_in_place()?
            .ok_or_else(|| GitError::Reference(format!("Fetched ref '{}' could not be resolved to an OID after fetch", remote_tracking_dst)))?;

            // Get the current local OID for the destination ref (re-read in case fetch updated it, though unlikely here)
            let local_ref = gix_repo.find_reference(&dst_ref)?;
            let local_oid = local_ref.peel_to_id_in_place()?
                .ok_or_else(|| GitError::Reference(format!("Local ref '{}' could not be resolved to an OID", dst_ref)))?;

        println!("Local OID for {}: {}", dst_ref_str, local_oid);
        println!("Fetched OID for {}: {}", remote_tracking_dst, fetched_oid);

            // --- Attempt Checkout/Merge using gitoxide's checkout ---
            println!("Attempting checkout/merge of fetched commit {} onto local {} ({})", fetched_oid, dst_ref_str, local_oid);
            
            // Configure checkout options
            // We want checkout to handle the merge if necessary.
            // The default options usually try to merge.
            let opts = checkout::Options {
                // strategy: checkout::Strategy::Merge, // Default strategy often includes merge
                // conflict_style: checkout::ConflictStyle::Merge, // Default
                ..Default::default()
            };

            // Perform the checkout, targeting the fetched remote commit
            let (checkout_outcome, maybe_new_index) = checkout::checkout(
                &gix_repo,
                opts,
                fetched_oid, // Target the commit we fetched and resolved
                &interrupt::IS_INTERRUPTED,
            ).map_err(|e| GitError::Repository(format!("Checkout/merge failed: {}", e), None))?;

            // --- Handle Checkout/Merge Outcome ---
            let mut conflicting_paths = Vec::new();
            // TODO: Properly extract conflicting paths from checkout_outcome.conflicts
            // The structure might provide more details than just paths.
            for conflict in &checkout_outcome.conflicts {
                 // Attempt to get path from worktree, fallback if worktree access fails
                 let path_str = gix_repo.worktree()
                     .map(|wt| wt.path().join(&conflict.path))
                     .map(|p| p.to_string_lossy().into_owned())
                     .unwrap_or_else(|| String::from_utf8_lossy(&conflict.path).into_owned()); // Fallback to raw bytes if path conversion fails
                 conflicting_paths.push(path_str);
            }

            // TODO: Handle checkout_outcome.collisions and checkout_outcome.errors more robustly
            if !checkout_outcome.errors.is_empty() {
                log::error!("Errors occurred during checkout/merge:");
                for (path, error) in &checkout_outcome.errors {
                    log::error!("  Path: {}, Error: {}", String::from_utf8_lossy(path), error);
                }
                // Decide if these errors constitute a merge failure
                // return Err(GitError::MergeFailure("Errors occurred during checkout/merge.".to_string()));
            }

            if !conflicting_paths.is_empty() {
                // Conflicts occurred. checkout() should have updated the index and working dir.
                println!("Merge conflict detected in files: {}", conflicting_paths.join(", "));
                eprintln!("Automatic merge failed; fix conflicts and then commit the result.");
                return Err(GitError::MergeConflict(conflicting_paths));
            } else {
                // Success (Fast-forward or Clean Merge)
                // Working dir and index were updated by checkout().
                // We now need to update the local ref (dst_ref) to the correct commit.

                // Determine the new HEAD OID after checkout/merge.
                // If checkout performed a merge, it might have created a new commit.
                // If it was a fast-forward, the target is simply remote_oid.
                // We need to check the actual HEAD ref now.
                let final_head_ref = gix_repo.head()?; // Get the Head object
                let final_oid = final_head_ref.peel_to_id_in_place()?
                    .ok_or_else(|| GitError::Repository("Could not resolve HEAD to OID after checkout/merge.".to_string(), None))?;

                // Check if the ref we intended to update (dst_ref) actually needs updating.
                // This handles cases where dst_ref wasn't HEAD, or if it was already up-to-date.
                let current_dst_ref_oid = gix_repo.find_reference(&dst_ref)?.peel_to_id_in_place()?;

                if current_dst_ref_oid != Some(final_oid) {
                    println!("Updating ref {} to {}", dst_ref_str, final_oid);
                    let edit = RefEdit {
                        change: Change::Update {
                            log: LogChange {
                                mode: gix::refs::log::RefLog::AndReference,
                                force_create_reflog: false,
                                // TODO: Improve log message (distinguish FF from Merge)
                                message: format!("pull: Update {} to {}", dst_ref_str, final_oid).into(),
                            },
                            // Use the OID we found before the checkout attempt
                            expected: PreviousValue::MustExistAndMatch(Target::Peeled(local_oid)),
                            new: Target::Peeled(final_oid),
                        },
                        name: dst_ref_str.try_into()?,
                        deref: false,
                    };
                    gix_repo.edit_references(std::iter::once(edit))?;
                    println!("Pull completed successfully. Working directory updated.");
                } else if final_oid == local_oid {
                    // This case might happen if checkout determined no merge was needed,
                    // effectively the 'Already up-to-date' case.
                    println!("Already up-to-date.");
                } else {
                    // dst_ref already pointed to the final_oid (e.g., HEAD was updated directly)
                    println!("Pull completed successfully. Working directory updated.");
                }
                Ok(())
            }
        // End of block previously wrapped by rt.block_on
    }
    
    /// Pull over HTTP
    fn pull_over_http(&self, repo: &Repository, remote_url: &str) -> Result<()> {
        println!("Pulling over HTTP");
        
        // Determine what to pull based on refspec
        let (src_ref, dst_ref) = self.parse_refspec()?;
        
        println!("Pulling {} into {}", src_ref, dst_ref);
        
        // TODO: In a real implementation, we would:
        // 1. Connect to the remote over HTTP
        // 2. Discover remote references
        // 3. Negotiate what needs to be fetched
        // 4. Fetch objects
        // 5. Update local references
        // 6. Merge the changes
        
        println!("Pull completed successfully (placeholder)");
        
        Ok(())
    }
    
    /// Parse the refspec into source and destination components
    fn parse_refspec(&self) -> Result<(String, String)> {
        match &self.refspec {
            Some(spec) => {
                // Parse the "src:dst" format
                let parts: Vec<&str> = spec.split(':').collect();
                if parts.len() == 2 {
                    Ok((parts[0].to_string(), parts[1].to_string()))
                } else if parts.len() == 1 {
                    // If only one part, use the same name for source and destination
                    let name = if parts[0].starts_with("refs/") {
                        parts[0].to_string()
                    } else {
                        format!("refs/heads/{}", parts[0])
                    };
                    Ok((name.clone(), name))
                } else {
                    Err(GitError::Reference(format!("Invalid refspec: {}", spec)))
                }
            },
            None => {
                // Use the current branch as the default refspec
                let refs_storage = Repository::open(&self.path)?.get_refs_storage().clone();
                
                // Get the current branch
                let head_ref = refs_storage.head()?
                    .ok_or_else(|| GitError::Reference("HEAD not found".to_string()))?;
                    
                // Extract the branch name
                let branch_name = if head_ref.starts_with("refs/heads/") {
                    head_ref["refs/heads/".len()..].to_string()
                } else {
                    return Err(GitError::Reference("HEAD is not a branch".to_string()));
                };
                
                // Use "branch:branch" format
                let full_ref = format!("refs/heads/{}", branch_name);
                Ok((full_ref.clone(), full_ref))
            }
        }
    }
}