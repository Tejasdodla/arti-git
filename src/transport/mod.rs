mod http;

pub use http::HttpConnection;

use crate::core::{Result, ObjectId, ObjectType};

/// Common trait for all transport types
pub trait Transport {
    /// Get references from the remote
    fn list_refs(&mut self) -> Result<Vec<(String, ObjectId)>>;
    
    /// Fetch objects from the remote
    fn fetch(&mut self, wants: &[ObjectId], haves: &[ObjectId]) -> Result<Vec<(ObjectType, Vec<u8>)>>;
    
    /// Push objects to the remote
    fn push(&mut self, objects: &[(ObjectType, Vec<u8>)], refs: &[(String, ObjectId)]) -> Result<()>;
}