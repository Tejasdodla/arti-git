use std::io::{self, Read, Write, Seek};
use bytes::Bytes;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use sha1::{Sha1, Digest};

use crate::core::{GitError, Result, ObjectId, ObjectType};

/// The header of a Git pack file
#[derive(Debug, Clone)]
pub struct PackHeader {
    /// Pack format version (currently 2 or 3)
    pub version: u32,
    /// Number of objects in the pack
    pub object_count: u32,
}

impl PackHeader {
    /// Create a new pack header
    pub fn new(version: u32, object_count: u32) -> Self {
        Self { version, object_count }
    }
    
    /// Read a pack header from a reader
    pub fn read_from<R: Read>(reader: &mut R) -> Result<Self> {
        let mut signature = [0u8; 4];
        reader.read_exact(&mut signature)
            .map_err(GitError::Io)?;
            
        if &signature != b"PACK" {
            return Err(GitError::InvalidObject("Invalid pack signature".to_string()));
        }
        
        let mut buf = [0u8; 4];
        
        reader.read_exact(&mut buf)
            .map_err(GitError::Io)?;
        let version = u32::from_be_bytes(buf);
        
        reader.read_exact(&mut buf)
            .map_err(GitError::Io)?;
        let object_count = u32::from_be_bytes(buf);
        
        Ok(Self { version, object_count })
    }
    
    /// Write the pack header to a writer
    pub fn write_to<W: Write>(&self, writer: &mut W) -> Result<()> {
        writer.write_all(b"PACK")
            .map_err(GitError::Io)?;
            
        writer.write_all(&self.version.to_be_bytes())
            .map_err(GitError::Io)?;
            
        writer.write_all(&self.object_count.to_be_bytes())
            .map_err(GitError::Io)?;
            
        Ok(())
    }
}

/// An entry in a Git pack file
#[derive(Debug)]
pub struct PackEntry {
    /// The type of object
    pub obj_type: ObjectType,
    /// The object ID
    pub id: ObjectId,
    /// The uncompressed data
    pub data: Bytes,
    /// The base object ID for delta-encoded objects
    pub base_id: Option<ObjectId>,
}

impl PackEntry {
    /// Create a new pack entry
    pub fn new(obj_type: ObjectType, id: ObjectId, data: Bytes) -> Self {
        Self {
            obj_type,
            id,
            data,
            base_id: None,
        }
    }
    
    /// Create a delta-encoded pack entry
    pub fn new_delta(obj_type: ObjectType, id: ObjectId, data: Bytes, base_id: ObjectId) -> Self {
        Self {
            obj_type,
            id,
            data,
            base_id: Some(base_id),
        }
    }
}

/// A Git pack file
#[derive(Debug)]
pub struct Pack {
    /// The header of the pack
    pub header: PackHeader,
    /// The entries in the pack
    pub entries: Vec<PackEntry>,
}

impl Pack {
    /// Create a new empty pack
    pub fn new() -> Self {
        Self {
            header: PackHeader::new(2, 0),
            entries: Vec::new(),
        }
    }
    
    /// Add an entry to the pack
    pub fn add_entry(&mut self, entry: PackEntry) {
        self.entries.push(entry);
        self.header.object_count += 1;
    }
    
    /// Read a pack file from a reader
    pub fn read_from<R: Read>(reader: &mut R) -> Result<Self> {
        let header = PackHeader::read_from(reader)?;
        let mut entries = Vec::with_capacity(header.object_count as usize);
        
        // TODO: Implement full pack parsing
        // This is just a placeholder - actual implementation would
        // read and parse all objects in the pack file
        
        Ok(Self { header, entries })
    }
    
    /// Write the pack to a writer
    pub fn write_to<W: Write>(&self, writer: &mut W) -> Result<ObjectId> {
        // Create a hasher to calculate the pack checksum
        let mut hasher = Sha1::new();
        let mut tee_writer = TeeWriter { writer, hasher: &mut hasher };

        // Ensure header reflects the actual number of entries
        let mut header = self.header.clone();
        header.object_count = self.entries.len() as u32;

        // Write the header
        header.write_to(&mut tee_writer)?;

        // Write all entries
        for entry in &self.entries {
            // Determine the pack object type code
            let type_code = match entry.obj_type {
                ObjectType::Commit => 1,
                ObjectType::Tree => 2,
                ObjectType::Blob => 3,
                ObjectType::Tag => 4,
                // TODO: Handle delta types (OBJ_OFS_DELTA, OBJ_REF_DELTA)
                _ => return Err(GitError::InvalidObject(format!("Unsupported object type for packing: {:?}", entry.obj_type))),
            };

            // Write the type and size header (variable length encoding)
            let size = entry.data.len();
            let mut header_byte = (type_code << 4) | (size & 0x0F) as u8;
            let mut remaining_size = size >> 4;
            let mut header_bytes = Vec::new();

            while remaining_size > 0 {
                header_bytes.push(header_byte | 0x80); // Set continuation bit
                header_byte = (remaining_size & 0x7F) as u8;
                remaining_size >>= 7;
            }
            header_bytes.push(header_byte); // Last byte without continuation bit
            tee_writer.write_all(&header_bytes)?;

            // TODO: Handle delta base object ID writing if applicable

            // Write the compressed object data
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&entry.data)?;
            let compressed_data = encoder.finish()?;
            tee_writer.write_all(&compressed_data)?;
        }
        
        // Calculate and write the checksum
        let hash = tee_writer.hasher.finalize();
        let mut hash_bytes = [0u8; 20];
        hash_bytes.copy_from_slice(&hash);
        
        writer.write_all(&hash_bytes)
            .map_err(GitError::Io)?;
            
        Ok(ObjectId::new(hash_bytes))
    }
}

/// A writer that also feeds data to a hasher
struct TeeWriter<'a, W: Write> {
    writer: W,
    hasher: &'a mut Sha1,
}

impl<'a, W: Write> Write for TeeWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.writer.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
    
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}