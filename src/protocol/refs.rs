use std::fmt;

use crate::core::{GitError, Result, ObjectId};

/// A Git reference (branch, tag, etc.)
#[derive(Debug, Clone)]
pub struct Reference {
    /// The name of the reference (e.g., "refs/heads/main")
    pub name: String,
    /// The object ID that the reference points to
    pub target: ObjectId,
    /// Whether this is a symbolic reference
    pub symbolic: bool,
    /// Peeled target (for annotated tags)
    pub peeled: Option<ObjectId>,
}

impl Reference {
    /// Create a new direct reference
    pub fn new(name: &str, target: ObjectId) -> Self {
        Self {
            name: name.to_string(),
            target,
            symbolic: false,
            peeled: None,
        }
    }
    
    /// Create a new symbolic reference
    pub fn symbolic(name: &str, target: &str, target_id: ObjectId) -> Self {
        Self {
            name: name.to_string(),
            target: target_id,
            symbolic: true,
            peeled: None,
        }
    }
    
    /// Check if this reference is a branch
    pub fn is_branch(&self) -> bool {
        self.name.starts_with("refs/heads/")
    }
    
    /// Check if this reference is a tag
    pub fn is_tag(&self) -> bool {
        self.name.starts_with("refs/tags/")
    }
    
    /// Check if this reference is a remote tracking branch
    pub fn is_remote(&self) -> bool {
        self.name.starts_with("refs/remotes/")
    }
    
    /// Get the short name of the reference
    pub fn short_name(&self) -> &str {
        if self.is_branch() {
            &self.name["refs/heads/".len()..]
        } else if self.is_tag() {
            &self.name["refs/tags/".len()..]
        } else if self.is_remote() {
            &self.name["refs/remotes/".len()..]
        } else {
            &self.name
        }
    }
    
    /// Parse a reference from a string in the format "<sha> <refname>"
    pub fn parse(line: &str) -> Result<Self> {
        let parts: Vec<&str> = line.trim().split_whitespace().collect();
        
        if parts.len() < 2 {
            return Err(GitError::Reference(format!("Invalid ref format: {}", line)));
        }
        
        let target = ObjectId::from_str(parts[0])
            .map_err(|_| GitError::Reference(format!("Invalid object ID: {}", parts[0])))?;
            
        let name = parts[1].to_string();
        let mut reference = Self::new(&name, target);
        
        // Check for peeled object (for annotated tags)
        if parts.len() > 2 && parts[2] == "^{}" && parts.len() > 3 {
            let peeled = ObjectId::from_str(parts[3])
                .map_err(|_| GitError::Reference(format!("Invalid peeled object ID: {}", parts[3])))?;
            reference.peeled = Some(peeled);
        }
        
        Ok(reference)
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.target, self.name)
    }
}