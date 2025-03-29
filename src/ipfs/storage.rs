use std::sync::Arc;
use std::fmt;
use bytes::Bytes;
use tokio::sync::RwLock;
use gix_hash::ObjectId;

use crate::core::{GitError, Result, ObjectType};
use super::client::IpfsClient;
use super::config::IpfsConfig;

/// IPFS object storage error
#[derive(Debug)]
pub enum IpfsStorageError {
    /// Object not found
    NotFound(String),
    /// Network error
    Network(String),
    /// Serialization error
    Serialization(String),
    /// Invalid object
    InvalidObject(String),
}

impl fmt::Display for IpfsStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(msg) => write!(f, "Object not found: {}", msg),
            Self::Network(msg) => write!(f, "Network error: {}", msg),
            Self::Serialization(msg) => write!(f, "Serialization error: {}", msg),
            Self::InvalidObject(msg) => write!(f, "Invalid object: {}", msg),
        }
    }
}

impl std::error::Error for IpfsStorageError {}

/// Mapping between Git object IDs and IPFS content IDs
#[derive(Debug, Clone)]
struct ObjectMapping {
    /// Git object ID
    git_id: ObjectId,
    /// IPFS content ID (CID)
    ipfs_cid: String,
    /// Object type
    object_type: ObjectType,
}

/// Provider for Git objects stored in IPFS
pub trait IpfsObjectProvider: Send + Sync {
    /// Get a Git object from IPFS
    async fn get_object(&self, id: &ObjectId) -> Result<(ObjectType, Bytes)>;
    
    /// Store a Git object in IPFS
    async fn store_object(&self, object_type: ObjectType, data: &[u8]) -> Result<ObjectId>;
    
    /// Check if an object exists
    async fn has_object(&self, id: &ObjectId) -> bool;
    
    /// Get the IPFS CID for a Git object
    async fn get_object_cid(&self, id: &ObjectId) -> Result<String>;
}

/// IPFS-based object storage for Git
pub struct IpfsObjectStorage {
    /// IPFS client
    client: Arc<IpfsClient>,
    
    /// Object mappings (Git object ID to IPFS CID)
    mappings: Arc<RwLock<Vec<ObjectMapping>>>,
}

impl IpfsObjectStorage {
    /// Create a new IPFS object storage
    pub async fn new(client: Arc<IpfsClient>) -> Result<Self> {
        Ok(Self {
            client,
            mappings: Arc::new(RwLock::new(Vec::new())),
        })
    }
    
    /// Add a mapping between a Git object ID and an IPFS CID
    async fn add_mapping(&self, git_id: ObjectId, ipfs_cid: String, object_type: ObjectType) {
        let mapping = ObjectMapping {
            git_id,
            ipfs_cid,
            object_type,
        };
        
        let mut mappings = self.mappings.write().await;
        mappings.push(mapping);
    }
    
    /// Find a mapping by Git object ID
    async fn find_mapping_by_git_id(&self, id: &ObjectId) -> Option<ObjectMapping> {
        let mappings = self.mappings.read().await;
        mappings.iter()
               .find(|m| &m.git_id == id)
               .cloned()
    }
}

impl IpfsObjectProvider for IpfsObjectStorage {
    async fn get_object(&self, id: &ObjectId) -> Result<(ObjectType, Bytes)> {
        // Find the mapping for this object
        let mapping = self.find_mapping_by_git_id(id).await
            .ok_or_else(|| GitError::NotFound(format!("Object not found: {}", id)))?;
        
        // Get the data from IPFS
        let data = self.client.get_file(&mapping.ipfs_cid).await?;
        
        Ok((mapping.object_type, data))
    }
    
    async fn store_object(&self, object_type: ObjectType, data: &[u8]) -> Result<ObjectId> {
        // Add object data to IPFS
        let cid = self.client.add_bytes(data).await?;
        
        // Calculate Git object ID
        let header = format!("{} {}\0", object_type.to_string(), data.len());
        let mut content = Vec::with_capacity(header.len() + data.len());
        content.extend_from_slice(header.as_bytes());
        content.extend_from_slice(data);
        
        let hash = gix_hash::Kind::Sha1.hash(&content);
        let object_id = ObjectId::from_hash(hash);
        
        // Add mapping
        self.add_mapping(object_id, cid, object_type).await;
        
        Ok(object_id)
    }
    
    async fn has_object(&self, id: &ObjectId) -> bool {
        self.find_mapping_by_git_id(id).await.is_some()
    }
    
    async fn get_object_cid(&self, id: &ObjectId) -> Result<String> {
        // Find the mapping for this object
        let mapping = self.find_mapping_by_git_id(id).await
            .ok_or_else(|| GitError::NotFound(format!("Object not found: {}", id)))?;
        
        Ok(mapping.ipfs_cid.clone())
    }
}

// Extension to convert from ObjectType enum to string
trait ObjectTypeExt {
    fn to_string(&self) -> &'static str;
}

impl ObjectTypeExt for ObjectType {
    fn to_string(&self) -> &'static str {
        match self {
            ObjectType::Blob => "blob",
            ObjectType::Tree => "tree",
            ObjectType::Commit => "commit",
            ObjectType::Tag => "tag",
        }
    }
}