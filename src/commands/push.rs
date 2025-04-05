use std::path::{Path, PathBuf};
use tokio::runtime::Runtime;
use std::collections::HashMap;

use crate::core::{GitError, Result, ObjectId};
use crate::core::ObjectType;
use crate::repository::Repository;
use crate::transport::{TorConnection, AsyncRemoteConnection};
// Imports for gitoxide pack generation
use gix::odb::FindExt;
use gix::object::Kind as GixKind;
use gix::hash::ObjectId as GixObjectId;
use gix::odb::pack::bundle::write::{Error as BundleWriteError, Options as BundleWriteOptions, Progress as BundleWriteProgress, Outcome as BundleWriteOutcome};
use gix::progress::Discard as ProgressDiscard;
use gix::refs::transaction::PreviousValue;
use gix::refs::Target;
use std::io::sink;
use bytes::Bytes; // Keep for potential use in transport or ObjectId conversion
use crate::protocol::PackEntry; // Keep PackEntry if needed elsewhere or for ObjectId conversion

/// Implements the `push` command functionality
pub struct PushCommand {
    /// Remote name
    remote: String,
    /// Refspec for pushing (e.g., "main:main")
    refspec: Option<String>,
    /// Repository path
    path: PathBuf,
    /// Whether to use anonymous mode over Tor
    anonymous: bool,
}

impl PushCommand {
    /// Create a new push command
    pub fn new(remote: &str, refspec: Option<&str>, path: &Path, anonymous: bool) -> Self {
        Self {
            remote: remote.to_string(),
            refspec: refspec.map(|s| s.to_string()),
            path: path.to_path_buf(),
            anonymous,
        }
    }
    
    /// Execute the push command
    pub fn execute(&self) -> Result<()> {
        // Open the repository
        let repo = Repository::open(&self.path)?;
        
        // Get the remote URL
        let config = repo.get_config();
        let remote_url = config.get(&format!("remote.{}.url", self.remote))
            .ok_or_else(|| GitError::Reference(format!("Remote '{}' not found", self.remote)))?;
        
        println!("Pushing to {} ({})", self.remote, remote_url);
        
        if self.anonymous {
            self.push_over_tor(&repo, remote_url)
        } else {
            self.push_over_http(&repo, remote_url)
        }
    }
    
    /// Push over Tor network
    fn push_over_tor(&self, repo: &Repository, remote_url: &str) -> Result<()> {
        println!("Pushing over Tor network");
        
        // Create a Tokio runtime
        let rt = Runtime::new()
            .map_err(|e| GitError::Transport(format!("Failed to create runtime: {}", e)))?;
            
        // Execute the push operation in the runtime
        rt.block_on(async {
            // Create and initialize the Tor connection
            let mut tor_conn = TorConnection::new(remote_url)?;
            
            println!("Bootstrapping Tor circuit (this may take a moment)...");
            tor_conn.init().await?;
            
            println!("Connected to Tor network");
            
            // Determine what to push based on refspec
            let (src_ref, dst_ref) = self.parse_refspec()?;
            
            // Get the local ref
            // Open the gitoxide repository instance
            let gix_repo = gix::open(&repo.path())
                .map_err(|e| GitError::Repository(format!("Failed to open gitoxide repository: {}", e), Some(repo.path().to_path_buf())))?;

            // Resolve the local ref OID using gitoxide
            let local_oid = gix_repo.find_reference(&src_ref)?
                .peel_to_id_in_place()?
                .ok_or_else(|| GitError::Reference(format!("Local ref '{}' could not be resolved to an OID", src_ref)))?;

            println!("Pushing {} to {}", src_ref, dst_ref);
            println!("Local OID: {}", local_oid);

            // --- Basic Negotiation: Get Remote Refs ---
            println!("Getting remote refs for negotiation...");
            // We need the transport's receive_pack handshake part here.
            // Let's reuse the discover_refs logic from TorConnection for now,
            // although ideally push negotiation uses receive-pack service directly.
            let remote_refs_map: std::collections::HashMap<String, gix::hash::ObjectId> = tor_conn.list_refs_async().await?
                .into_iter()
                .filter_map(|(name, oid_res)| oid_res.ok().map(|oid| (name, oid.into()))) // Convert our ObjectId to gix::hash::ObjectId
                .collect();

            let remote_oid = remote_refs_map.get(&dst_ref).cloned();
            if let Some(r_oid) = remote_oid {
                println!("Remote OID for {}: {}", dst_ref, r_oid);
            } else {
                println!("Remote ref {} not found, pushing all objects.", dst_ref);
            }
            // --- Object Discovery ---
            println!("Collecting objects to push...");
            let mut objects_to_send_oids = std::collections::HashSet::new();
            let mut object_data_buffer = Vec::new(); // Reusable buffer

            // Use revwalk to find all reachable objects from the local OID
            let mut walk = local_oid.ancestors(|oid, buf| gix_repo.objects.find(oid, buf).map(|d| (d.kind, d.data)))?;

            // Exclude commits reachable from the remote OID (if it exists)
            if let Some(remote_commit_oid) = remote_oid {
                 // Mark remote commits as 'uninteresting' to stop the walk there
                 walk.add_uninteresting(remote_commit_oid)?;
            }
            // Collect commits and recursively collect their trees and blobs
            while let Some(commit_info) = walk.next() {
                let commit_info = commit_info?;
                collect_commit_objects(&gix_repo, commit_info.id(), &mut objects_to_send_oids, &mut object_data_buffer)?;
            }
            println!("Need to send {} unique objects.", objects_to_send_oids.len());

            // --- Packfile Generation (using gitoxide) ---
            println!("Generating packfile using gitoxide for {} objects...", objects_to_send_oids.len());

            let options = BundleWriteOptions {
                thread_limit: None, // Use default thread limit
                iteration_mode: gix::pack::bundle::write::IterationMode::Verify,
                index_kind: gix::pack::index::Version::V2,
            };

            // Prepare 'have' refs (what the remote has)
            let have_refs: Vec<GixObjectId> = remote_oid.into_iter().collect();
            // Prepare 'want' refs (what we are pushing) - just the single local OID
            let want_refs = vec![local_oid];

            // Generate the packfile data
            // We need to map the OIDs to Result<Oid, Error> for the writer function's constraints
            // Using Infallible as the error type as our OIDs are already validated/collected.
            let object_ids_iter = objects_to_send_oids.iter().map(|oid| Ok::<_, std::convert::Infallible>(*oid));
            let have_refs_iter = have_refs.into_iter().map(Ok::<_, std::convert::Infallible>);
            let want_refs_iter = want_refs.into_iter().map(Ok::<_, std::convert::Infallible>);

            // Use Box<dyn Iterator...> to erase the concrete iterator types for the function arguments
            let have_refs_dyn_iter: Option<Box<dyn Iterator<Item = std::result::Result<GixObjectId, Box<dyn std::error::Error + Send + Sync + 'static>>>>> =
                Some(Box::new(have_refs_iter.map(|r| r.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync + 'static>))));
            let want_refs_dyn_iter: Option<Box<dyn Iterator<Item = std::result::Result<GixObjectId, Box<dyn std::error::Error + Send + Sync + 'static>>>>> =
                Some(Box::new(want_refs_iter.map(|r| r.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync + 'static>))));

            let pack_result = gix::pack::bundle::write::to_vec(
                object_ids_iter, // Provide OIDs to include
                &gix_repo.objects, // The object database
                ProgressDiscard, // Progress reporting (discard for now)
                options,
                have_refs_dyn_iter, // 'have' refs
                want_refs_dyn_iter, // 'want' refs
            ).map_err(|e| GitError::PackGeneration(format!("Failed to generate packfile using gitoxide: {}", e)))?;

            // Extract the pack data (Vec<u8>)
            let pack_data = pack_result.0;
            println!("Generated packfile of {} bytes using gitoxide.", pack_data.len());

            // --- Push Packfile ---
            // Ensure local_oid (gix::hash::ObjectId) is converted to crate::core::ObjectId if needed.
            // Assuming an `impl From<gix::hash::ObjectId> for crate::core::ObjectId` exists.
            let refs_to_update = vec![(dst_ref.clone(), local_oid.into())];
            println!("Pushing {} objects (in packfile) and {} refs", objects_to_send_oids.len(), refs_to_update.len()); // Use count from collected oids

            // Push the generated packfile data and refs
            // TODO: Update tor_conn.push_objects_async to accept packfile data instead of individual objects
            // For now, assuming it takes raw bytes - this will likely need adjustment in transport/tor.rs
            // Push the generated packfile data and refs
            // TODO: Ensure tor_conn.push_packfile_async accepts &[u8] for pack_data
            // and Vec<(String, crate::core::ObjectId)> for refs_to_update.
            let push_result = tor_conn.push_packfile_async(&pack_data, &refs_to_update).await;

            // Handle the result from the transport layer
            match push_result {
                Ok(()) => {
                    println!("Push completed successfully.");
                    Ok(())
                }
                Err(GitError::Protocol(msg)) => {
                    // Provide specific feedback based on the protocol error from the transport layer
                    eprintln!("Push failed: {}", msg); // Print to stderr
                    Err(GitError::Protocol(msg)) // Propagate the specific error
                }
                Err(e) => {
                    // Propagate other transport or unexpected errors
                    eprintln!("Push failed due to an unexpected error: {}", e);
                    Err(e)
                }
            }
        })
    }
    
    /// Recursively collect objects (commit, tree, blob) starting from a commit OID.
    fn collect_commit_objects(
        gix_repo: &gix::Repository,
        commit_oid: gix::hash::ObjectId,
        objects_to_send: &mut std::collections::HashSet<gix::hash::ObjectId>,
        buffer: &mut Vec<u8>,
    ) -> Result<()> {
        if !objects_to_send.insert(commit_oid) {
            return Ok(()); // Already processed or added
        }
        println!("  Collecting commit: {}", commit_oid);
    
        // Find the commit object to get its tree
        let commit_obj = gix_repo.objects.find_commit(&commit_oid, buffer)?;
        let tree_oid = commit_obj.tree();
    
        // Recursively collect objects from the tree
        collect_tree_objects(gix_repo, tree_oid, objects_to_send, buffer)?;
    
        Ok(())
    }
    
    /// Recursively collect objects (tree, blob) starting from a tree OID.
    fn collect_tree_objects(
        gix_repo: &gix::Repository,
        tree_oid: gix::hash::ObjectId,
        objects_to_send: &mut std::collections::HashSet<gix::hash::ObjectId>,
        buffer: &mut Vec<u8>,
    ) -> Result<()> {
        if !objects_to_send.insert(tree_oid) {
            return Ok(()); // Already processed or added
        }
        println!("    Collecting tree: {}", tree_oid);
    
        // Find and parse the tree object
        let tree_obj = gix_repo.objects.find_tree(&tree_oid, buffer)?;
        for entry in tree_obj.iter() {
            let entry = entry?;
            if entry.mode().is_tree() { // Recurse into subtrees
                collect_tree_objects(gix_repo, entry.oid(), objects_to_send, buffer)?;
            } else if entry.mode().is_blob() || entry.mode().is_blob_executable() { // Collect blobs
                if objects_to_send.insert(entry.oid()) {
                     println!("      Collecting blob: {}", entry.oid());
                }
            }
            // Ignore submodules for now
        }
        Ok(())
    }
    
    /// Push over HTTP
    fn push_over_http(&self, repo: &Repository, remote_url: &str) -> Result<()> {
        println!("Pushing over HTTP");
        
        // Determine what to push based on refspec
        let (src_ref, dst_ref) = self.parse_refspec()?;
        
        // Get the local ref
        let refs_storage = repo.get_refs_storage();
        let local_ref_value = refs_storage.get_ref(&src_ref)?
            .ok_or_else(|| GitError::Reference(format!("Local ref '{}' not found", src_ref)))?;
            
        println!("Pushing {} to {}", src_ref, dst_ref);
        
        // TODO: In a real implementation, we would:
        // 1. Connect to the remote over HTTP
        // 2. Negotiate what needs to be pushed
        // 3. Create and send a pack file with the objects
        // 4. Update remote references
        
        println!("Push completed successfully (placeholder)");
        
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
                    Ok((parts[0].to_string(), parts[0].to_string()))
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
                Ok((format!("refs/heads/{}", branch_name), format!("refs/heads/{}", branch_name)))
            }
        }
    }
}