mod error;
mod object;

pub use error::{GitError, Result};
pub use object::{ObjectId, ObjectType};

/// Trait for Git object storage
pub trait ObjectStorage {
    fn read_object(&self, id: &ObjectId) -> Result<(ObjectType, Vec<u8>)>;
    fn write_object(&mut self, obj_type: ObjectType, data: &[u8]) -> Result<ObjectId>;
    fn has_object(&self, id: &ObjectId) -> Result<bool>;
}

/// Trait for remote connections
pub trait RemoteConnection {
    fn fetch_objects(&mut self, wants: &[ObjectId], haves: &[ObjectId]) 
        -> Result<Vec<(ObjectType, ObjectId, bytes::Bytes)>>;
    fn push_objects(&mut self, objects: &[(ObjectType, ObjectId, bytes::Bytes)]) -> Result<()>;
}