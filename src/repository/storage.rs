use std::path::{Path, PathBuf};
use std::fs;
use std::io::{self, Read, Write};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::core::{GitError, Result, ObjectId, ObjectType, ObjectStorage};

/// File system implementation of Git object storage
pub struct FileSystemObjectStore {
    path: PathBuf,
}

impl FileSystemObjectStore {
    /// Create a new file system object store
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    
    /// Get the path for an object file
    fn object_path(&self, id: &ObjectId) -> PathBuf {
        let id_hex = id.to_hex();
        let dir = &id_hex[0..2];
        let file = &id_hex[2..];
        self.path.join(dir).join(file)
    }
}

impl ObjectStorage for FileSystemObjectStore {
    fn read_object(&self, id: &ObjectId) -> Result<(ObjectType, Vec<u8>)> {
        let path = self.object_path(id);
        
        if !path.exists() {
            return Err(GitError::NotFound(id.clone()));
        }
        
        // Open file and create zlib decoder
        let file = fs::File::open(path).map_err(GitError::Io)?;
        let mut decoder = ZlibDecoder::new(file);
        
        // Read all content
        let mut content = Vec::new();
        decoder.read_to_end(&mut content).map_err(GitError::Io)?;
        
        // Parse header
        let header_end = content
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| GitError::InvalidObject(format!("Invalid object format: {}", id)))?;
            
        let header = std::str::from_utf8(&content[..header_end])
            .map_err(|_| GitError::InvalidObject(format!("Invalid object header: {}", id)))?;
            
        let parts: Vec<&str> = header.split(' ').collect();
        if parts.len() != 2 {
            return Err(GitError::InvalidObject(format!("Invalid object header format: {}", id)));
        }
        
        let obj_type = ObjectType::from_str(parts[0])
            .ok_or_else(|| GitError::InvalidObject(format!("Unknown object type: {}", parts[0])))?;
            
        let size = parts[1].parse::<usize>()
            .map_err(|_| GitError::InvalidObject(format!("Invalid object size: {}", parts[1])))?;
            
        // Extract object content
        let content = content[header_end + 1..].to_vec();
        if content.len() != size {
            return Err(GitError::InvalidObject(format!(
                "Object size mismatch: expected {}, got {}", 
                size, 
                content.len()
            )));
        }
        
        Ok((obj_type, content))
    }
    
    fn write_object(&mut self, obj_type: ObjectType, data: &[u8]) -> Result<ObjectId> {
        // Prepare header
        let header = format!("{} {}", obj_type.as_str(), data.len());
        let id = ObjectId::compute(obj_type, data);
        let path = self.object_path(&id);
        
        // Ensure directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(GitError::Io)?;
        }
        
        // Check if object already exists
        if path.exists() {
            return Ok(id);
        }
        
        // Create temporary file
        let dir = path.parent().unwrap();
        let mut temp_file = tempfile::NamedTempFile::new_in(dir)
            .map_err(GitError::Io)?;
            
        // Create zlib encoder and write data
        {
            let mut encoder = ZlibEncoder::new(temp_file.as_file_mut(), Compression::default());
            encoder.write_all(header.as_bytes()).map_err(GitError::Io)?;
            encoder.write_all(&[0]).map_err(GitError::Io)?;
            encoder.write_all(data).map_err(GitError::Io)?;
            encoder.finish().map_err(GitError::Io)?;
        }
        
        // Rename temporary file to final path
        temp_file.persist(path).map_err(|e| GitError::Io(io::Error::new(
            io::ErrorKind::Other,
            format!("Failed to persist temporary file: {}", e)
        )))?;
        
        Ok(id)
    }
    
    fn has_object(&self, id: &ObjectId) -> Result<bool> {
        Ok(self.object_path(id).exists())
    }
}