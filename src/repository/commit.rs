use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};
use chrono::{DateTime, Utc};

use crate::core::{GitError, Result, ObjectId, ObjectType};
use crate::crypto::{Identity, Signer, Verifier, SignatureError};

/// Represents a Git commit object
#[derive(Debug, Clone)]
pub struct Commit {
    /// The tree object ID
    pub tree: ObjectId,
    /// Parent commit IDs
    pub parents: Vec<ObjectId>,
    /// The author of the commit
    pub author: CommitSignature,
    /// The committer of the commit
    pub committer: CommitSignature,
    /// The commit message
    pub message: String,
    /// GPG signature if the commit is signed
    pub signature: Option<String>,
}

impl Commit {
    /// Create a new commit
    pub fn new(
        tree: ObjectId,
        parents: Vec<ObjectId>,
        author: CommitSignature,
        committer: CommitSignature,
        message: String,
    ) -> Self {
        Self {
            tree,
            parents,
            author,
            committer,
            message,
            signature: None,
        }
    }
    
    /// Sign the commit using the given identity
    pub fn sign<I: Identity>(&mut self, identity: &I) -> Result<()> {
        // Generate the commit data without signature
        let commit_data = self.format_for_signing();
        
        // Sign the commit data
        let signature_bytes = identity.sign(commit_data.as_bytes())
            .map_err(|e| GitError::Signature(format!("Failed to sign commit: {}", e)))?;
            
        // Format the signature as base64
        let signature = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD, 
            &signature_bytes
        );
        
        // Store the signature
        self.signature = Some(format!("-----BEGIN ARTGIT SIGNATURE-----\n{}\n-----END ARTGIT SIGNATURE-----", signature));
        
        Ok(())
    }
    
    /// Verify the commit signature
    pub fn verify<V: Verifier>(&self, verifier: &V) -> Result<bool> {
        // Extract the signature data
        let signature_data = match &self.signature {
            Some(sig) => {
                let sig = sig.trim();
                if !sig.starts_with("-----BEGIN ARTGIT SIGNATURE-----") || 
                   !sig.ends_with("-----END ARTGIT SIGNATURE-----") {
                    return Err(GitError::Signature("Invalid signature format".to_string()));
                }
                
                let sig_content = sig
                    .trim_start_matches("-----BEGIN ARTGIT SIGNATURE-----")
                    .trim_end_matches("-----END ARTGIT SIGNATURE-----")
                    .trim();
                    
                base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD, 
                    sig_content
                ).map_err(|e| GitError::Signature(format!("Invalid signature encoding: {}", e)))?
            },
            None => return Err(GitError::Signature("Commit is not signed".to_string())),
        };
        
        // Generate the commit data for verification
        let commit_data = self.format_for_signing();
        
        // Verify the signature
        verifier.verify(commit_data.as_bytes(), &signature_data)
            .map_err(|e| GitError::Signature(format!("Signature verification failed: {}", e)))
    }
    
    /// Format the commit for signing (without the signature)
    fn format_for_signing(&self) -> String {
        let mut content = String::new();
        
        content.push_str(&format!("tree {}\n", self.tree));
        
        for parent in &self.parents {
            content.push_str(&format!("parent {}\n", parent));
        }
        
        content.push_str(&format!("author {}\n", self.author));
        content.push_str(&format!("committer {}\n", self.committer));
        
        // Add the message
        content.push('\n');
        content.push_str(&self.message);
        
        content
    }
    
    /// Format the complete commit including signature
    pub fn format(&self) -> String {
        let mut content = self.format_for_signing();
        
        // Add the signature if present
        if let Some(signature) = &self.signature {
            content.push_str("\ngpgsig ");
            let sig_lines = signature.lines();
            for (i, line) in sig_lines.enumerate() {
                if i > 0 {
                    content.push_str(" ");
                }
                content.push_str(line);
                content.push('\n');
            }
        }
        
        content
    }
    
    /// Parse a commit from raw data
    pub fn parse(data: &[u8]) -> Result<Self> {
        let content = std::str::from_utf8(data)
            .map_err(|_| GitError::InvalidObject("Commit is not valid UTF-8".to_string()))?;
            
        let mut lines = content.lines();
        let mut tree = None;
        let mut parents = Vec::new();
        let mut author = None;
        let mut committer = None;
        let mut signature = None;
        
        // Parse headers
        while let Some(line) = lines.next() {
            if line.is_empty() {
                break;
            }
            
            if line.starts_with("tree ") {
                let tree_sha = &line["tree ".len()..];
                tree = Some(ObjectId::from_str(tree_sha)
                    .map_err(|_| GitError::InvalidObject(format!("Invalid tree ID: {}", tree_sha)))?);
            } else if line.starts_with("parent ") {
                let parent_sha = &line["parent ".len()..];
                let parent = ObjectId::from_str(parent_sha)
                    .map_err(|_| GitError::InvalidObject(format!("Invalid parent ID: {}", parent_sha)))?;
                parents.push(parent);
            } else if line.starts_with("author ") {
                let author_str = &line["author ".len()..];
                author = Some(CommitSignature::parse(author_str)?);
            } else if line.starts_with("committer ") {
                let committer_str = &line["committer ".len()..];
                committer = Some(CommitSignature::parse(committer_str)?);
            } else if line.starts_with("gpgsig ") {
                let mut sig = line["gpgsig ".len()..].to_string();
                
                // Parse multi-line signature
                while let Some(line) = lines.next() {
                    if line.starts_with(" ") {
                        sig.push('\n');
                        sig.push_str(&line[1..]);
                    } else {
                        break;
                    }
                }
                
                signature = Some(sig);
            }
        }
        
        // Parse message (the rest of the commit)
        let message = lines.collect::<Vec<&str>>().join("\n");
        
        // Validate required fields
        let tree = tree.ok_or_else(|| GitError::InvalidObject("Missing tree".to_string()))?;
        let author = author.ok_or_else(|| GitError::InvalidObject("Missing author".to_string()))?;
        let committer = committer.ok_or_else(|| GitError::InvalidObject("Missing committer".to_string()))?;
        
        Ok(Self {
            tree,
            parents,
            author,
            committer,
            message,
            signature,
        })
    }
}

/// Represents a Git commit signature (author or committer)
#[derive(Debug, Clone)]
pub struct CommitSignature {
    /// The name of the author/committer
    pub name: String,
    /// The email of the author/committer
    pub email: String,
    /// The timestamp when the commit was created
    pub time: DateTime<Utc>,
    /// The timezone offset in minutes
    pub tz_offset: i32,
}

impl CommitSignature {
    /// Create a new commit signature
    pub fn new(name: &str, email: &str, time: DateTime<Utc>, tz_offset: i32) -> Self {
        Self {
            name: name.to_string(),
            email: email.to_string(),
            time,
            tz_offset,
        }
    }
    
    /// Create a commit signature with the current time
    pub fn now(name: &str, email: &str) -> Self {
        Self::new(name, email, Utc::now(), 0)
    }
    
    /// Parse a commit signature from a string
    pub fn parse(s: &str) -> Result<Self> {
        // Format: "Name <email> timestamp timezone"
        let parts: Vec<&str> = s.rsplitn(3, ' ').collect();
        if parts.len() < 3 {
            return Err(GitError::InvalidObject(format!("Invalid signature format: {}", s)));
        }
        
        let tz_str = parts[0];
        let timestamp_str = parts[1];
        let name_email = parts[2];
        
        // Parse name and email
        let email_start = name_email.rfind('<')
            .ok_or_else(|| GitError::InvalidObject(format!("Missing email start in signature: {}", s)))?;
        let email_end = name_email.rfind('>')
            .ok_or_else(|| GitError::InvalidObject(format!("Missing email end in signature: {}", s)))?;
            
        if email_start >= email_end {
            return Err(GitError::InvalidObject(format!("Invalid email format in signature: {}", s)));
        }
        
        let name = name_email[..email_start].trim().to_string();
        let email = name_email[email_start + 1..email_end].to_string();
        
        // Parse timestamp
        let timestamp = timestamp_str.parse::<i64>()
            .map_err(|_| GitError::InvalidObject(format!("Invalid timestamp: {}", timestamp_str)))?;
        let time = DateTime::from_timestamp(timestamp, 0)
            .ok_or_else(|| GitError::InvalidObject(format!("Invalid timestamp value: {}", timestamp)))?;
        
        // Parse timezone offset
        let tz_offset = if tz_str.len() == 5 {
            let sign = if tz_str.starts_with('+') { 1 } else { -1 };
            let hours = tz_str[1..3].parse::<i32>()
                .map_err(|_| GitError::InvalidObject(format!("Invalid timezone hours: {}", tz_str)))?;
            let minutes = tz_str[3..5].parse::<i32>()
                .map_err(|_| GitError::InvalidObject(format!("Invalid timezone minutes: {}", tz_str)))?;
                
            sign * (hours * 60 + minutes)
        } else {
            return Err(GitError::InvalidObject(format!("Invalid timezone format: {}", tz_str)));
        };
        
        Ok(Self { name, email, time, tz_offset })
    }
}

impl fmt::Display for CommitSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let timestamp = self.time.timestamp();
        
        // Format timezone offset
        let tz_hours = self.tz_offset.abs() / 60;
        let tz_minutes = self.tz_offset.abs() % 60;
        let tz_sign = if self.tz_offset >= 0 { '+' } else { '-' };
        
        write!(
            f, 
            "{} <{}> {} {}{:02}{:02}", 
            self.name, 
            self.email, 
            timestamp, 
            tz_sign, 
            tz_hours, 
            tz_minutes
        )
    }
}

/// Represents a signed Git tag
#[derive(Debug, Clone)]
pub struct Tag {
    /// The object ID that this tag points to
    pub target: ObjectId,
    /// The type of object being tagged
    pub target_type: ObjectType,
    /// The name of the tag
    pub name: String,
    /// The tagger information
    pub tagger: CommitSignature,
    /// The tag message
    pub message: String,
    /// The signature if the tag is signed
    pub signature: Option<String>,
}

impl Tag {
    /// Create a new tag
    pub fn new(
        target: ObjectId,
        target_type: ObjectType,
        name: &str,
        tagger: CommitSignature,
        message: &str,
    ) -> Self {
        Self {
            target,
            target_type,
            name: name.to_string(),
            tagger,
            message: message.to_string(),
            signature: None,
        }
    }
    
    /// Sign the tag using the given identity
    pub fn sign<I: Identity>(&mut self, identity: &I) -> Result<()> {
        // Similar to commit signing
        let tag_data = self.format_for_signing();
        
        let signature_bytes = identity.sign(tag_data.as_bytes())
            .map_err(|e| GitError::Signature(format!("Failed to sign tag: {}", e)))?;
            
        let signature = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD, 
            &signature_bytes
        );
        
        self.signature = Some(format!("-----BEGIN ARTGIT SIGNATURE-----\n{}\n-----END ARTGIT SIGNATURE-----", signature));
        
        Ok(())
    }
    
    /// Format the tag for signing
    fn format_for_signing(&self) -> String {
        let mut content = String::new();
        
        content.push_str(&format!("object {}\n", self.target));
        content.push_str(&format!("type {}\n", self.target_type.as_str()));
        content.push_str(&format!("tag {}\n", self.name));
        content.push_str(&format!("tagger {}\n", self.tagger));
        
        content.push('\n');
        content.push_str(&self.message);
        
        content
    }
}