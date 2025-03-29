use std::path::{Path, PathBuf};

use crate::core::{GitError, Result, ObjectId};
use crate::repository::{Repository, Signature};
use crate::crypto::SignatureProvider;

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
    pub fn execute(self) -> Result<ObjectId> {
        // Open the repository
        let repo = Repository::open(&self.path)?;
        
        // Create author and committer signatures
        let config = repo.get_config();
        let author_name = config.get("user.name")
            .unwrap_or_else(|| "Anonymous".to_string());
        let author_email = config.get("user.email")
            .unwrap_or_else(|| "anonymous@localhost".to_string());
            
        let author = Signature::new(&author_name, &author_email, chrono::Utc::now());
        let committer = author.clone();
        
        println!("Creating commit with message: {}", self.message);
        
        // If signing is enabled, handle crypto operations
        if self.sign {
            println!("Signing commit with anonymous identity");
            
            // Create a signature provider
            let mut signature_provider = SignatureProvider::new();
            
            // If a custom onion address is provided, use it as identity
            if let Some(onion) = self.onion_address {
                println!("Using custom onion identity: {}", onion);
                signature_provider.use_onion_address(onion)?;
            } else {
                // Generate a new identity or use the default one
                println!("Using default anonymous identity");
            }
            
            // Create the commit with signature
            let commit_id = repo.create_commit_signed(
                "HEAD", 
                &author, 
                &committer, 
                &self.message, 
                &[], 
                &signature_provider,
            )?;
            
            println!("Created signed commit: {}", commit_id);
            Ok(commit_id)
        } else {
            // Create the commit without signature
            let commit_id = repo.create_commit(
                "HEAD", 
                &author, 
                &committer, 
                &self.message, 
                &[],
            )?;
            
            println!("Created commit: {}", commit_id);
            Ok(commit_id)
        }
    }
}