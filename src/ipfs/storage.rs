use std::sync::Arc;
use std::fmt;
use std::path::{Path, PathBuf};
use std::fs;
use std::io::Write;
use std::collections::{HashMap, HashSet};
use bytes::{Bytes, BytesMut};
use tokio::sync::{RwLock, Mutex};
use gix_hash::ObjectId;
use serde::{Serialize, Deserialize};
use sha2::{Sha256, Digest};
use rayon::prelude::*;

use crate::core::{GitError, Result, ObjectType, io_err};
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
    /// Storage error
    Storage(String),
    /// Chunking error
    Chunking(String),
}

impl fmt::Display for IpfsStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(msg) => write!(f, "Object not found: {}", msg),
            Self::Network(msg) => write!(f, "Network error: {}", msg),
            Self::Serialization(msg) => write!(f, "Serialization error: {}", msg),
            Self::InvalidObject(msg) => write!(f, "Invalid object: {}", msg),
            Self::Storage(msg) => write!(f, "Storage error: {}", msg),
            Self::Chunking(msg) => write!(f, "Chunking error: {}", msg),
        }
    }
}

impl std::error::Error for IpfsStorageError {}

impl From<IpfsStorageError> for GitError {
    fn from(err: IpfsStorageError) -> Self {
        GitError::IpfsError(err.to_string())
    }
}

/// Content hashing algorithm for deduplication
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ContentHashAlgorithm {
    /// SHA-256 hash
    Sha256,
    /// Blake3 hash
    Blake3,
}

impl Default for ContentHashAlgorithm {
    fn default() -> Self {
        Self::Sha256
    }
}

/// Settings for object chunking strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkingStrategy {
    /// Minimum chunk size in bytes
    pub min_chunk_size: usize,
    /// Maximum chunk size in bytes
    pub max_chunk_size: usize,
    /// Target chunk size in bytes
    pub target_chunk_size: usize,
    /// Content-defined chunking window size
    pub window_size: usize,
    /// Chunking algorithm to use
    pub algorithm: ChunkingAlgorithm,
}

impl Default for ChunkingStrategy {
    fn default() -> Self {
        Self {
            min_chunk_size: 16 * 1024,      // 16 KB
            max_chunk_size: 4 * 1024 * 1024, // 4 MB
            target_chunk_size: 256 * 1024,   // 256 KB
            window_size: 64,                // 64 bytes window for rolling hash
            algorithm: ChunkingAlgorithm::FastCDC,
        }
    }
}

/// Available chunking algorithms
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ChunkingAlgorithm {
    /// Fixed-size chunking
    FixedSize,
    /// Content-Defined Chunking using Fast CDC algorithm
    FastCDC,
    /// Content-Defined Chunking using Rabin algorithm
    Rabin,
}

/// Mapping between Git object IDs and IPFS content IDs with additional metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObjectMapping {
    /// Git object ID
    git_id: String,
    /// IPFS content ID (CID)
    ipfs_cid: String,
    /// Object type
    object_type: String,
    /// Size of the object in bytes
    size: usize,
    /// Content hash for deduplication (SHA-256 by default)
    content_hash: Option<String>,
    /// Chunked flag - indicates if the object is stored as chunks
    is_chunked: bool,
    /// If chunked, list of chunk CIDs
    chunk_cids: Vec<String>,
    /// Timestamp when the object was added
    #[serde(with = "chrono::serde::ts_seconds")]
    timestamp: chrono::DateTime<chrono::Utc>,
}

impl ObjectMapping {
    fn new(git_id: &ObjectId, ipfs_cid: String, object_type: ObjectType, size: usize) -> Self {
        Self {
            git_id: git_id.to_string(),
            ipfs_cid,
            object_type: object_type.to_string().to_string(),
            size,
            content_hash: None,
            is_chunked: false,
            chunk_cids: Vec::new(),
            timestamp: chrono::Utc::now(),
        }
    }

    fn with_content_hash(git_id: &ObjectId, ipfs_cid: String, object_type: ObjectType, size: usize, 
                        content_hash: String) -> Self {
        Self {
            git_id: git_id.to_string(),
            ipfs_cid,
            object_type: object_type.to_string().to_string(),
            size,
            content_hash: Some(content_hash),
            is_chunked: false,
            chunk_cids: Vec::new(),
            timestamp: chrono::Utc::now(),
        }
    }

    fn chunked(git_id: &ObjectId, ipfs_cid: String, object_type: ObjectType, size: usize, 
              chunk_cids: Vec<String>) -> Self {
        Self {
            git_id: git_id.to_string(),
            ipfs_cid,
            object_type: object_type.to_string().to_string(),
            size,
            content_hash: None,
            is_chunked: true,
            chunk_cids,
            timestamp: chrono::Utc::now(),
        }
    }
}

/// A chunk of object data with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObjectChunk {
    /// Hash of the chunk content
    content_hash: String,
    /// IPFS CID for the chunk
    ipfs_cid: String,
    /// Size of the chunk
    size: usize,
    /// Number of references to this chunk
    ref_count: usize,
}

/// Cache statistics for monitoring
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct CacheStats {
    /// Cache hits
    pub hits: usize,
    /// Cache misses
    pub misses: usize,
    /// Objects stored
    pub objects_stored: usize,
    /// Total bytes stored
    pub total_bytes_stored: usize,
    /// Deduplication savings in bytes
    pub dedup_savings: usize,
    /// Number of chunked objects
    pub chunked_objects: usize,
    /// Number of unique chunks stored
    pub unique_chunks: usize,
    /// Number of total chunks (including duplicates)
    pub total_chunks: usize,
}

impl CacheStats {
    /// Calculate cache hit ratio
    pub fn hit_ratio(&self) -> f64 {
        if self.hits + self.misses == 0 {
            0.0
        } else {
            self.hits as f64 / (self.hits + self.misses) as f64
        }
    }

    /// Calculate deduplication ratio
    pub fn dedup_ratio(&self) -> f64 {
        if self.total_bytes_stored == 0 {
            0.0
        } else {
            self.dedup_savings as f64 / self.total_bytes_stored as f64
        }
    }

    /// Calculate chunk deduplication ratio
    pub fn chunk_dedup_ratio(&self) -> f64 {
        if self.total_chunks == 0 {
            0.0
        } else {
            (self.total_chunks - self.unique_chunks) as f64 / self.total_chunks as f64
        }
    }
}

/// Advanced storage settings for IPFS object storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpfsStorageSettings {
    /// Whether to use content-addressed deduplication
    pub use_deduplication: bool,
    /// Content hash algorithm to use for deduplication
    pub content_hash_algorithm: ContentHashAlgorithm,
    /// Whether to use chunking for large objects
    pub use_chunking: bool,
    /// Size threshold for chunking (in bytes)
    pub chunking_threshold: usize,
    /// Chunking strategy
    pub chunking_strategy: ChunkingStrategy,
    /// Whether to pin objects in IPFS
    pub pin_objects: bool,
    /// Maximum time to wait for IPFS operations (in seconds)
    pub timeout_seconds: u64,
    /// Whether to use background uploads
    pub use_background_uploads: bool,
    /// Maximum size of the local cache (in bytes, 0 = unlimited)
    pub max_cache_size: usize,
}

impl Default for IpfsStorageSettings {
    fn default() -> Self {
        Self {
            use_deduplication: true,
            content_hash_algorithm: ContentHashAlgorithm::Sha256,
            use_chunking: true,
            chunking_threshold: 1024 * 1024, // 1 MB
            chunking_strategy: ChunkingStrategy::default(),
            pin_objects: true,
            timeout_seconds: 120,
            use_background_uploads: true,
            max_cache_size: 1024 * 1024 * 1024, // 1 GB
        }
    }
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
    
    /// Get cache statistics
    fn get_stats(&self) -> CacheStats;

    /// Store multiple objects in batch
    async fn store_objects_batch(&self, objects: Vec<(ObjectType, Bytes)>) -> Result<Vec<ObjectId>>;

    /// Get multiple objects in batch
    async fn get_objects_batch(&self, ids: &[ObjectId]) -> Result<Vec<(ObjectId, ObjectType, Bytes)>>;
}

/// Background upload task information
struct BackgroundUploadTask {
    /// The Git object ID
    git_id: ObjectId,
    /// The object type
    object_type: ObjectType,
    /// The data to upload
    data: Bytes,
    /// Status of the upload
    status: UploadStatus,
}

/// Upload status for background tasks
#[derive(Debug, Clone, Copy, PartialEq)]
enum UploadStatus {
    /// Upload is pending
    Pending,
    /// Upload is in progress
    InProgress,
    /// Upload has completed successfully
    Completed,
    /// Upload has failed
    Failed,
}

/// IPFS-based object storage for Git
pub struct IpfsObjectStorage {
    /// IPFS client
    client: Arc<IpfsClient>,
    
    /// Object mappings (Git object ID to IPFS CID)
    mappings: Arc<RwLock<HashMap<String, ObjectMapping>>>,
    
    /// Chunk mappings (content hash to chunk info)
    chunks: Arc<RwLock<HashMap<String, ObjectChunk>>>,
    
    /// Content hash to Git object ID mapping for deduplication
    content_to_git: Arc<RwLock<HashMap<String, String>>>,
    
    /// Local cache directory
    cache_dir: PathBuf,
    
    /// Mappings file path
    mappings_file: PathBuf,
    
    /// Chunks file path
    chunks_file: PathBuf,
    
    /// Enable local caching of objects
    cache_enabled: bool,
    
    /// Cache statistics
    stats: Arc<RwLock<CacheStats>>,

    /// Advanced storage settings
    settings: IpfsStorageSettings,

    /// Background upload tasks
    background_tasks: Arc<Mutex<HashMap<String, BackgroundUploadTask>>>,
}

impl IpfsObjectStorage {
    /// Create a new IPFS object storage
    pub async fn new(client: Arc<IpfsClient>) -> Result<Self> {
        // Use default cache location in user's data directory
        let mut cache_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("~/.local/share"));
        cache_dir.push("arti-git");
        cache_dir.push("ipfs-cache");
        
        Self::with_cache(client, cache_dir).await
    }
    
    /// Create a new IPFS object storage with a specific cache directory
    pub async fn with_cache(client: Arc<IpfsClient>, cache_dir: PathBuf) -> Result<Self> {
        Self::with_cache_and_settings(client, cache_dir, IpfsStorageSettings::default()).await
    }

    /// Create a new IPFS object storage with a specific cache directory and settings
    pub async fn with_cache_and_settings(
        client: Arc<IpfsClient>, 
        cache_dir: PathBuf,
        settings: IpfsStorageSettings
    ) -> Result<Self> {
        // Create cache directory if it doesn't exist
        fs::create_dir_all(&cache_dir)
            .map_err(|e| io_err(format!("Failed to create cache directory: {}", e), &cache_dir))?;
        
        let objects_dir = cache_dir.join("objects");
        fs::create_dir_all(&objects_dir)
            .map_err(|e| io_err(format!("Failed to create objects directory: {}", e), &objects_dir))?;
        
        let chunks_dir = cache_dir.join("chunks");
        fs::create_dir_all(&chunks_dir)
            .map_err(|e| io_err(format!("Failed to create chunks directory: {}", e), &chunks_dir))?;
        
        let mappings_file = cache_dir.join("mappings.json");
        let chunks_file = cache_dir.join("chunks.json");
        
        // Load existing mappings if available
        let mappings = if mappings_file.exists() {
            match fs::read_to_string(&mappings_file) {
                Ok(content) => {
                    match serde_json::from_str::<Vec<ObjectMapping>>(&content) {
                        Ok(list) => {
                            // Convert list to HashMap
                            let mut map = HashMap::new();
                            for mapping in list {
                                map.insert(mapping.git_id.clone(), mapping);
                            }
                            map
                        },
                        Err(e) => {
                            log::warn!("Failed to parse mappings file, starting with empty mappings: {}", e);
                            HashMap::new()
                        }
                    }
                },
                Err(e) => {
                    log::warn!("Failed to read mappings file, starting with empty mappings: {}", e);
                    HashMap::new()
                }
            }
        } else {
            HashMap::new()
        };

        // Load existing chunks if available
        let chunks = if chunks_file.exists() {
            match fs::read_to_string(&chunks_file) {
                Ok(content) => {
                    match serde_json::from_str::<Vec<ObjectChunk>>(&content) {
                        Ok(list) => {
                            // Convert list to HashMap
                            let mut map = HashMap::new();
                            for chunk in list {
                                map.insert(chunk.content_hash.clone(), chunk);
                            }
                            map
                        },
                        Err(e) => {
                            log::warn!("Failed to parse chunks file, starting with empty chunks: {}", e);
                            HashMap::new()
                        }
                    }
                },
                Err(e) => {
                    log::warn!("Failed to read chunks file, starting with empty chunks: {}", e);
                    HashMap::new()
                }
            }
        } else {
            HashMap::new()
        };

        // Build content hash to Git ID mapping for deduplication
        let mut content_to_git = HashMap::new();
        for (git_id, mapping) in &mappings {
            if let Some(content_hash) = &mapping.content_hash {
                content_to_git.insert(content_hash.clone(), git_id.clone());
            }
        }
        
        // Initialize stats based on loaded data
        let mut stats = CacheStats::default();
        stats.objects_stored = mappings.len();
        stats.unique_chunks = chunks.len();
        
        // Count total stored bytes and chunked objects
        for mapping in mappings.values() {
            stats.total_bytes_stored += mapping.size;
            if mapping.is_chunked {
                stats.chunked_objects += 1;
                stats.total_chunks += mapping.chunk_cids.len();
            }
        }
        
        log::info!("IPFS object storage initialized with {} existing mappings and {} chunks",
                  mappings.len(), chunks.len());
        
        Ok(Self {
            client,
            mappings: Arc::new(RwLock::new(mappings)),
            chunks: Arc::new(RwLock::new(chunks)),
            content_to_git: Arc::new(RwLock::new(content_to_git)),
            cache_dir,
            mappings_file,
            chunks_file,
            cache_enabled: true,
            stats: Arc::new(RwLock::new(stats)),
            settings,
            background_tasks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Set advanced storage settings
    pub fn with_settings(mut self, settings: IpfsStorageSettings) -> Self {
        self.settings = settings;
        self
    }
    
    /// Enable or disable local caching
    pub fn set_caching(&mut self, enabled: bool) {
        self.cache_enabled = enabled;
    }
    
    /// Save mappings to disk
    async fn save_mappings(&self) -> Result<()> {
        let mappings = self.mappings.read().await;
        let mappings_list: Vec<ObjectMapping> = mappings.values().cloned().collect();
        
        let json = serde_json::to_string_pretty(&mappings_list)
            .map_err(|e| GitError::IpfsError(format!("Failed to serialize mappings: {}", e)))?;
        
        // Write to a temporary file first, then rename for atomicity
        let temp_file = self.mappings_file.with_extension("tmp");
        fs::write(&temp_file, json)
            .map_err(|e| io_err(format!("Failed to write mappings file: {}", e), &temp_file))?;
        
        // Rename for atomic replacement
        fs::rename(&temp_file, &self.mappings_file)
            .map_err(|e| io_err(format!("Failed to rename mappings file: {}", e), &self.mappings_file))?;
        
        Ok(())
    }

    /// Save chunks to disk
    async fn save_chunks(&self) -> Result<()> {
        let chunks = self.chunks.read().await;
        let chunks_list: Vec<ObjectChunk> = chunks.values().cloned().collect();
        
        let json = serde_json::to_string_pretty(&chunks_list)
            .map_err(|e| GitError::IpfsError(format!("Failed to serialize chunks: {}", e)))?;
        
        // Write to a temporary file first, then rename for atomicity
        let temp_file = self.chunks_file.with_extension("tmp");
        fs::write(&temp_file, json)
            .map_err(|e| io_err(format!("Failed to write chunks file: {}", e), &temp_file))?;
        
        // Rename for atomic replacement
        fs::rename(&temp_file, &self.chunks_file)
            .map_err(|e| io_err(format!("Failed to rename chunks file: {}", e), &self.chunks_file))?;
        
        Ok(())
    }
    
    /// Get path for a cached object
    fn get_object_path(&self, id: &ObjectId) -> PathBuf {
        let id_str = id.to_string();
        let prefix = &id_str[0..2];
        let suffix = &id_str[2..];
        
        self.cache_dir.join("objects").join(prefix).join(suffix)
    }

    /// Get path for a cached chunk
    fn get_chunk_path(&self, content_hash: &str) -> PathBuf {
        let prefix = &content_hash[0..2];
        let suffix = &content_hash[2..];
        
        self.cache_dir.join("chunks").join(prefix).join(suffix)
    }
    
    /// Check if an object is in the local cache
    fn is_in_cache(&self, id: &ObjectId) -> bool {
        self.get_object_path(id).exists()
    }

    /// Check if a chunk is in the local cache
    fn is_chunk_in_cache(&self, content_hash: &str) -> bool {
        self.get_chunk_path(content_hash).exists()
    }
    
    /// Store an object in the local cache
    async fn store_in_cache(&self, id: &ObjectId, object_type: ObjectType, data: &[u8]) -> Result<()> {
        if !self.cache_enabled {
            return Ok(());
        }
        
        let object_path = self.get_object_path(id);
        
        // Create parent directory if it doesn't exist
        if let Some(parent) = object_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| io_err(format!("Failed to create directory: {}", e), parent))?;
        }
        
        // Write the object to disk
        let temp_path = object_path.with_extension("tmp");
        fs::write(&temp_path, data)
            .map_err(|e| io_err(format!("Failed to write cached object: {}", e), &temp_path))?;
        
        // Rename for atomic replacement
        fs::rename(&temp_path, &object_path)
            .map_err(|e| io_err(format!("Failed to rename cached object: {}", e), &object_path))?;
        
        Ok(())
    }

    /// Store a chunk in the local cache
    async fn store_chunk_in_cache(&self, content_hash: &str, data: &[u8]) -> Result<()> {
        if !self.cache_enabled {
            return Ok(());
        }
        
        let chunk_path = self.get_chunk_path(content_hash);
        
        // Create parent directory if it doesn't exist
        if let Some(parent) = chunk_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| io_err(format!("Failed to create directory: {}", e), parent))?;
        }
        
        // Write the chunk to disk
        let temp_path = chunk_path.with_extension("tmp");
        fs::write(&temp_path, data)
            .map_err(|e| io_err(format!("Failed to write cached chunk: {}", e), &temp_path))?;
        
        // Rename for atomic replacement
        fs::rename(&temp_path, &chunk_path)
            .map_err(|e| io_err(format!("Failed to rename cached chunk: {}", e), &chunk_path))?;
        
        Ok(())
    }
    
    /// Get an object from the local cache
    fn get_from_cache(&self, id: &ObjectId) -> Result<Bytes> {
        let object_path = self.get_object_path(id);
        
        fs::read(&object_path)
            .map(Bytes::from)
            .map_err(|e| io_err(format!("Failed to read cached object: {}", e), &object_path).into())
    }

    /// Get a chunk from the local cache
    fn get_chunk_from_cache(&self, content_hash: &str) -> Result<Bytes> {
        let chunk_path = self.get_chunk_path(content_hash);
        
        fs::read(&chunk_path)
            .map(Bytes::from)
            .map_err(|e| io_err(format!("Failed to read cached chunk: {}", e), &chunk_path).into())
    }

    /// Calculate content hash for deduplication
    fn calculate_content_hash(&self, data: &[u8]) -> String {
        match self.settings.content_hash_algorithm {
            ContentHashAlgorithm::Sha256 => {
                let mut hasher = Sha256::new();
                hasher.update(data);
                let result = hasher.finalize();
                hex::encode(result)
            },
            ContentHashAlgorithm::Blake3 => {
                let hash = blake3::hash(data);
                hash.to_hex().to_string()
            },
        }
    }

    /// Split data into chunks using the configured chunking strategy
    fn split_into_chunks(&self, data: &[u8]) -> Vec<Bytes> {
        // Skip chunking for small objects
        if data.len() < self.settings.chunking_threshold {
            return vec![Bytes::copy_from_slice(data)];
        }

        match self.settings.chunking_strategy.algorithm {
            ChunkingAlgorithm::FixedSize => {
                // Simple fixed-size chunking
                let chunk_size = self.settings.chunking_strategy.target_chunk_size;
                let mut chunks = Vec::new();
                let mut pos = 0;
                
                while pos < data.len() {
                    let end = std::cmp::min(pos + chunk_size, data.len());
                    chunks.push(Bytes::copy_from_slice(&data[pos..end]));
                    pos = end;
                }
                
                chunks
            },
            ChunkingAlgorithm::FastCDC => {
                // Content-defined chunking using FastCDC algorithm
                self.fast_cdc_chunking(data)
            },
            ChunkingAlgorithm::Rabin => {
                // Content-defined chunking using Rabin algorithm
                // For now, we'll just use fixed-size chunking as a fallback
                // In a real implementation, we would implement proper Rabin chunking
                let chunk_size = self.settings.chunking_strategy.target_chunk_size;
                let mut chunks = Vec::new();
                let mut pos = 0;
                
                while pos < data.len() {
                    let end = std::cmp::min(pos + chunk_size, data.len());
                    chunks.push(Bytes::copy_from_slice(&data[pos..end]));
                    pos = end;
                }
                
                chunks
            },
        }
    }

    /// Fast Content-Defined Chunking implementation
    fn fast_cdc_chunking(&self, data: &[u8]) -> Vec<Bytes> {
        let strategy = &self.settings.chunking_strategy;
        let min_size = strategy.min_chunk_size;
        let max_size = strategy.max_chunk_size;
        let avg_size = strategy.target_chunk_size;
        
        // Constants for FastCDC
        const GEAR_MASK: u32 = 0x0000_FFFF;
        const GEAR: u32 = 0x0000_8765;
        
        let normal_size = avg_size;
        let normal_bits = (avg_size as f64).log2() as u32;
        let mask_s = (1u32 << (normal_bits - 1)) - 1;
        let mask_l = (1u32 << normal_bits) - 1;
        
        let mut chunks = Vec::new();
        let mut i = 0;
        let mut last = 0;
        
        while i < data.len() {
            let mut fp: u32 = 0;
            let mut j = i;
            
            // Let's make sure we respect minimum and maximum chunk sizes
            let min_bound = last + min_size;
            let max_bound = std::cmp::min(last + max_size, data.len());
            
            if max_bound <= min_bound {
                // If we can't fit a minimum sized chunk, just add the rest of the data
                chunks.push(Bytes::copy_from_slice(&data[last..]));
                break;
            }
            
            let mut break_point = max_bound;
            
            // First scan to minimum boundary with higher threshold
            if j < min_bound {
                j = min_bound;
            }
            
            // Scan with lower threshold until target size
            let mut mask = if j < last + normal_size { mask_s } else { mask_l };
            
            while j < max_bound {
                fp = (fp << 1).wrapping_add((data[j] as u32) & GEAR_MASK);
                if (fp & mask) == 0 {
                    break_point = j;
                    break;
                }
                j += 1;
            }
            
            // Add the chunk
            chunks.push(Bytes::copy_from_slice(&data[last..break_point+1]));
            last = break_point + 1;
            i = last;
            
            // If we've added all data, we're done
            if last >= data.len() {
                break;
            }
        }
        
        chunks
    }

    /// Store chunks and return their CIDs
    async fn store_chunks(&self, chunks: &[Bytes]) -> Result<Vec<String>> {
        let mut chunk_cids = Vec::with_capacity(chunks.len());
        let mut unique_chunks = 0;
        
        // Process chunks in parallel using Rayon if there are multiple chunks
        if chunks.len() > 1 {
            // First, calculate content hashes and check which chunks we already have
            let content_hashes: Vec<String> = chunks.par_iter()
                .map(|chunk| self.calculate_content_hash(&chunk))
                .collect();
                
            // Check which chunks we already have
            let mut known_chunks = HashSet::new();
            {
                let chunks_map = self.chunks.read().await;
                for hash in &content_hashes {
                    if chunks_map.contains_key(hash) {
                        known_chunks.insert(hash.clone());
                    }
                }
            }
            
            // Process each chunk sequentially to store to IPFS
            for (i, chunk) in chunks.iter().enumerate() {
                let content_hash = &content_hashes[i];
                
                // Check if we already have this chunk
                if known_chunks.contains(content_hash) {
                    // Reuse existing chunk
                    let cid = {
                        let chunks_map = self.chunks.read().await;
                        chunks_map.get(content_hash).map(|c| c.ipfs_cid.clone())
                    };
                    
                    if let Some(cid) = cid {
                        // Update reference count
                        {
                            let mut chunks_map = self.chunks.write().await;
                            if let Some(chunk_info) = chunks_map.get_mut(content_hash) {
                                chunk_info.ref_count += 1;
                            }
                        }
                        
                        chunk_cids.push(cid);
                        continue;
                    }
                }
                
                // We need to store this chunk
                let cid = self.client.add_bytes(&chunk).await?;
                
                // Cache the chunk locally if enabled
                if self.cache_enabled {
                    if let Err(e) = self.store_chunk_in_cache(content_hash, &chunk).await {
                        log::warn!("Failed to cache chunk: {}", e);
                    }
                }
                
                // Add chunk mapping
                {
                    let mut chunks_map = self.chunks.write().await;
                    chunks_map.insert(content_hash.clone(), ObjectChunk {
                        content_hash: content_hash.clone(),
                        ipfs_cid: cid.clone(),
                        size: chunk.len(),
                        ref_count: 1,
                    });
                }
                
                chunk_cids.push(cid);
                unique_chunks += 1;
            }
        } else if chunks.len() == 1 {
            // For a single chunk, process directly
            let chunk = &chunks[0];
            let content_hash = self.calculate_content_hash(&chunk);
            
            // Check if we already have this chunk
            let existing_cid = {
                let chunks_map = self.chunks.read().await;
                chunks_map.get(&content_hash).map(|c| c.ipfs_cid.clone())
            };
            
            let cid = if let Some(existing_cid) = existing_cid {
                // Update reference count
                {
                    let mut chunks_map = self.chunks.write().await;
                    if let Some(chunk_info) = chunks_map.get_mut(&content_hash) {
                        chunk_info.ref_count += 1;
                    }
                }
                
                existing_cid
            } else {
                // Store new chunk
                let cid = self.client.add_bytes(&chunk).await?;
                
                // Cache the chunk locally if enabled
                if self.cache_enabled {
                    if let Err(e) = self.store_chunk_in_cache(&content_hash, &chunk).await {
                        log::warn!("Failed to cache chunk: {}", e);
                    }
                }
                
                // Add chunk mapping
                {
                    let mut chunks_map = self.chunks.write().await;
                    chunks_map.insert(content_hash.clone(), ObjectChunk {
                        content_hash,
                        ipfs_cid: cid.clone(),
                        size: chunk.len(),
                        ref_count: 1,
                    });
                }
                
                unique_chunks += 1;
                cid
            };
            
            chunk_cids.push(cid);
        }
        
        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.unique_chunks += unique_chunks;
            stats.total_chunks += chunks.len();
        }
        
        // Save chunks to disk periodically
        if self.cache_enabled && unique_chunks > 0 {
            if let Err(e) = self.save_chunks().await {
                log::warn!("Failed to save chunk mappings: {}", e);
            }
        }
        
        Ok(chunk_cids)
    }

    /// Reassemble object from chunks
    async fn reassemble_from_chunks(&self, chunks_cids: &[String]) -> Result<Bytes> {
        // Preallocate a buffer for the full object
        let mut total_size = 0;
        {
            let chunks_map = self.chunks.read().await;
            for cid in chunks_cids {
                // Find the chunk by CID
                let mut found = false;
                for chunk in chunks_map.values() {
                    if chunk.ipfs_cid == *cid {
                        total_size += chunk.size;
                        found = true;
                        break;
                    }
                }
                
                if !found {
                    log::warn!("Chunk with CID {} not found in local mapping, size calculation may be off", cid);
                }
            }
        }
        
        let mut buffer = BytesMut::with_capacity(total_size);
        
        // Retrieve each chunk and append to buffer
        for cid in chunks_cids {
            // First check if we have content hash for this CID
            let content_hash = {
                let chunks_map = self.chunks.read().await;
                let mut hash = None;
                for chunk in chunks_map.values() {
                    if chunk.ipfs_cid == *cid {
                        hash = Some(chunk.content_hash.clone());
                        break;
                    }
                }
                hash
            };
            
            if let Some(hash) = content_hash {
                // Check if chunk is in local cache
                if self.cache_enabled && self.is_chunk_in_cache(&hash) {
                    match self.get_chunk_from_cache(&hash) {
                        Ok(data) => {
                            buffer.extend_from_slice(&data);
                            continue;
                        },
                        Err(e) => {
                            log::warn!("Failed to get chunk from cache, falling back to IPFS: {}", e);
                        }
                    }
                }
            }
            
            // Get the chunk from IPFS
            match self.client.get_file(cid).await {
                Ok(data) => {
                    // Cache the chunk if we have its content hash
                    if self.cache_enabled && content_hash.is_some() {
                        if let Err(e) = self.store_chunk_in_cache(&content_hash.unwrap(), &data).await {
                            log::warn!("Failed to cache chunk: {}", e);
                        }
                    }
                    
                    buffer.extend_from_slice(&data);
                },
                Err(e) => {
                    return Err(GitError::IpfsError(format!("Failed to get chunk from IPFS: {}", e)));
                }
            }
        }
        
        Ok(buffer.freeze())
    }
    
    /// Add a mapping between a Git object ID and an IPFS CID
    async fn add_mapping(&self, git_id: &ObjectId, ipfs_cid: String, object_type: ObjectType, size: usize) -> Result<()> {
        let mapping = ObjectMapping::new(git_id, ipfs_cid, object_type, size);
        
        {
            let mut mappings = self.mappings.write().await;
            mappings.insert(git_id.to_string(), mapping);
        }
        
        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.objects_stored += 1;
            stats.total_bytes_stored += size;
        }
        
        // Save mappings to disk periodically
        // For better performance, we could implement a background task for this
        if self.cache_enabled {
            self.save_mappings().await?;
        }
        
        Ok(())
    }

    /// Add a mapping with content hash for deduplication
    async fn add_mapping_with_content_hash(
        &self, 
        git_id: &ObjectId, 
        ipfs_cid: String, 
        object_type: ObjectType, 
        size: usize,
        content_hash: String
    ) -> Result<()> {
        let mapping = ObjectMapping::with_content_hash(git_id, ipfs_cid, object_type, size, content_hash.clone());
        
        {
            let mut mappings = self.mappings.write().await;
            mappings.insert(git_id.to_string(), mapping);
            
            // Add to content hash mapping for deduplication
            let mut content_map = self.content_to_git.write().await;
            content_map.insert(content_hash, git_id.to_string());
        }
        
        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.objects_stored += 1;
            stats.total_bytes_stored += size;
        }
        
        // Save mappings to disk periodically
        if self.cache_enabled {
            self.save_mappings().await?;
        }
        
        Ok(())
    }

    /// Add a mapping for a chunked object
    async fn add_chunked_mapping(
        &self, 
        git_id: &ObjectId, 
        ipfs_cid: String, 
        object_type: ObjectType, 
        size: usize,
        chunk_cids: Vec<String>
    ) -> Result<()> {
        let mapping = ObjectMapping::chunked(git_id, ipfs_cid, object_type, size, chunk_cids);
        
        {
            let mut mappings = self.mappings.write().await;
            mappings.insert(git_id.to_string(), mapping);
        }
        
        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.objects_stored += 1;
            stats.total_bytes_stored += size;
            stats.chunked_objects += 1;
        }
        
        // Save mappings to disk periodically
        if self.cache_enabled {
            self.save_mappings().await?;
        }
        
        Ok(())
    }

    /// Submit a background upload task
    async fn submit_background_upload(
        &self,
        object_type: ObjectType,
        data: Bytes
    ) -> Result<ObjectId> {
        // Calculate Git object ID
        let header = format!("{} {}\0", object_type.to_string(), data.len());
        let mut content = Vec::with_capacity(header.len() + data.len());
        content.extend_from_slice(header.as_bytes());
        content.extend_from_slice(&data);
        
        let hash = gix_hash::Kind::Sha1.hash(&content);
        let object_id = ObjectId::from_hash(hash);
        
        // Check if we already have this object
        if self.has_object(&object_id).await {
            log::debug!("Object {} already exists, skipping background upload", object_id);
            return Ok(object_id);
        }
        
        // Create a background task
        {
            let mut tasks = self.background_tasks.lock().await;
            tasks.insert(object_id.to_string(), BackgroundUploadTask {
                git_id: object_id.clone(),
                object_type,
                data: data.clone(),
                status: UploadStatus::Pending,
            });
        }
        
        // Start a background task to upload the object
        let object_storage = self.clone();
        let object_id_clone = object_id.clone();
        
        tokio::spawn(async move {
            // Mark as in progress
            {
                let mut tasks = object_storage.background_tasks.lock().await;
                if let Some(task) = tasks.get_mut(&object_id_clone.to_string()) {
                    task.status = UploadStatus::InProgress;
                }
            }
            
            // Attempt to store the object
            let result = object_storage.store_object_internal(object_type, &data).await;
            
            // Update task status
            {
                let mut tasks = object_storage.background_tasks.lock().await;
                match result {
                    Ok(_) => {
                        if let Some(task) = tasks.get_mut(&object_id_clone.to_string()) {
                            task.status = UploadStatus::Completed;
                        }
                    },
                    Err(e) => {
                        log::error!("Background upload failed for object {}: {}", object_id_clone, e);
                        if let Some(task) = tasks.get_mut(&object_id_clone.to_string()) {
                            task.status = UploadStatus::Failed;
                        }
                    }
                }
            }
        });
        
        Ok(object_id)
    }

    /// Internal method to actually store an object
    async fn store_object_internal(&self, object_type: ObjectType, data: &[u8]) -> Result<ObjectId> {
        // Calculate Git object ID
        let header = format!("{} {}\0", object_type.to_string(), data.len());
        let mut content = Vec::with_capacity(header.len() + data.len());
        content.extend_from_slice(header.as_bytes());
        content.extend_from_slice(data);
        
        let hash = gix_hash::Kind::Sha1.hash(&content);
        let object_id = ObjectId::from_hash(hash);
        
        // Check if we already have this object
        if self.has_object(&object_id).await {
            log::debug!("Object {} already exists, skipping storage", object_id);
            return Ok(object_id);
        }
        
        // Try deduplication by content if enabled
        if self.settings.use_deduplication {
            let content_hash = self.calculate_content_hash(data);
            let existing_git_id = {
                let content_map = self.content_to_git.read().await;
                content_map.get(&content_hash).cloned()
            };
            
            if let Some(existing_id) = existing_git_id {
                // We found a duplicate by content hash! Create a new mapping pointing to the same CID
                let existing_mapping = {
                    let mappings = self.mappings.read().await;
                    mappings.get(&existing_id).cloned()
                };
                
                if let Some(mapping) = existing_mapping {
                    log::debug!("Content deduplication: Object {} has same content as {}", object_id, existing_id);
                    
                    // Create a new mapping with the same IPFS CID
                    if mapping.is_chunked {
                        self.add_chunked_mapping(&object_id, mapping.ipfs_cid.clone(), object_type, data.len(), mapping.chunk_cids.clone()).await?;
                    } else {
                        self.add_mapping_with_content_hash(&object_id, mapping.ipfs_cid.clone(), object_type, data.len(), content_hash).await?;
                    }
                    
                    // Update deduplication stats
                    {
                        let mut stats = self.stats.write().await;
                        stats.dedup_savings += data.len();
                    }
                    
                    // Store in local cache if enabled
                    if self.cache_enabled {
                        if let Err(e) = self.store_in_cache(&object_id, object_type, data).await {
                            log::warn!("Failed to cache object: {}", e);
                        }
                    }
                    
                    return Ok(object_id);
                }
            }
        }
        
        // Use chunking for large objects if enabled
        if self.settings.use_chunking && data.len() >= self.settings.chunking_threshold {
            log::debug!("Chunking object {} ({} bytes)", object_id, data.len());
            
            // Split data into chunks
            let chunks = self.split_into_chunks(data);
            log::debug!("Object {} split into {} chunks", object_id, chunks.len());
            
            // Store all chunks
            let chunk_cids = self.store_chunks(&chunks).await?;
            
            // Create a DAG to link all chunks
            let dag_cid = if chunks.len() > 1 {
                // Create an IPFS DAG with links to all chunks
                // This is a simplified implementation; a real one would use proper IPLD formats
                let dag = serde_json::json!({
                    "data": {
                        "type": object_type.to_string(),
                        "size": data.len(),
                        "chunks": chunk_cids.clone()
                    }
                });
                
                self.client.add_json(&dag).await?
            } else {
                // If there's only one chunk, use its CID directly
                chunk_cids[0].clone()
            };
            
            // Add mapping for the chunked object
            self.add_chunked_mapping(&object_id, dag_cid, object_type, data.len(), chunk_cids).await?;
            
            // Store in local cache if enabled
            if self.cache_enabled {
                if let Err(e) = self.store_in_cache(&object_id, object_type, data).await {
                    log::warn!("Failed to cache object: {}", e);
                }
            }
            
            Ok(object_id)
        } else {
            // Store directly for small objects
            log::debug!("Storing object {} directly ({} bytes)", object_id, data.len());
            
            // Add object data to IPFS
            let cid = self.client.add_bytes(data).await?;
            log::debug!("Stored object {} with CID {}", object_id, cid);
            
            // Calculate content hash for deduplication if enabled
            if self.settings.use_deduplication {
                let content_hash = self.calculate_content_hash(data);
                self.add_mapping_with_content_hash(&object_id, cid, object_type, data.len(), content_hash).await?;
            } else {
                // Add mapping without content hash
                self.add_mapping(&object_id, cid, object_type, data.len()).await?;
            }
            
            // Store in local cache if enabled
            if self.cache_enabled {
                if let Err(e) = self.store_in_cache(&object_id, object_type, data).await {
                    log::warn!("Failed to cache object: {}", e);
                }
            }
            
            Ok(object_id)
        }
    }

    /// Clone the storage for internal use
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            mappings: self.mappings.clone(),
            chunks: self.chunks.clone(),
            content_to_git: self.content_to_git.clone(),
            cache_dir: self.cache_dir.clone(),
            mappings_file: self.mappings_file.clone(),
            chunks_file: self.chunks_file.clone(),
            cache_enabled: self.cache_enabled,
            stats: self.stats.clone(),
            settings: self.settings.clone(),
            background_tasks: self.background_tasks.clone(),
        }
    }
}

impl IpfsObjectProvider for IpfsObjectStorage {
    async fn get_object(&self, id: &ObjectId) -> Result<(ObjectType, Bytes)> {
        // Check if we have a mapping for this object
        let mapping = {
            let mappings = self.mappings.read().await;
            mappings.get(&id.to_string()).cloned()
        };
        
        match mapping {
            Some(mapping) => {
                // Check if object is chunked
                if mapping.is_chunked {
                    log::debug!("Getting chunked object {} from IPFS", id);
                    
                    // Reassemble from chunks
                    let data = self.reassemble_from_chunks(&mapping.chunk_cids).await?;
                    
                    // Store in local cache if enabled
                    if self.cache_enabled {
                        // Convert object type string back to enum
                        let object_type = match mapping.object_type.as_str() {
                            "blob" => ObjectType::Blob,
                            "tree" => ObjectType::Tree,
                            "commit" => ObjectType::Commit,
                            "tag" => ObjectType::Tag,
                            _ => return Err(GitError::IpfsError(format!("Invalid object type: {}", mapping.object_type)))
                        };
                        
                        if let Err(e) = self.store_in_cache(id, object_type, &data).await {
                            log::warn!("Failed to cache object: {}", e);
                        }
                    }
                    
                    // Convert object type string back to enum
                    let object_type = match mapping.object_type.as_str() {
                        "blob" => ObjectType::Blob,
                        "tree" => ObjectType::Tree,
                        "commit" => ObjectType::Commit,
                        "tag" => ObjectType::Tag,
                        _ => return Err(GitError::IpfsError(format!("Invalid object type: {}", mapping.object_type)))
                    };
                    
                    return Ok((object_type, data));
                }
                
                // Try to get the object from the local cache first
                if self.cache_enabled && self.is_in_cache(id) {
                    match self.get_from_cache(id) {
                        Ok(data) => {
                            // Update cache hit stats
                            {
                                let mut stats = self.stats.write().await;
                                stats.hits += 1;
                            }
                            
                            // Convert object type string back to enum
                            let object_type = match mapping.object_type.as_str() {
                                "blob" => ObjectType::Blob,
                                "tree" => ObjectType::Tree,
                                "commit" => ObjectType::Commit,
                                "tag" => ObjectType::Tag,
                                _ => return Err(GitError::IpfsError(format!("Invalid object type: {}", mapping.object_type)))
                            };
                            
                            return Ok((object_type, data));
                        }
                        Err(e) => {
                            log::warn!("Failed to get object from cache, trying IPFS: {}", e);
                        }
                    }
                }
                
                // Get the data from IPFS
                log::debug!("Fetching object {} from IPFS with CID {}", id, mapping.ipfs_cid);
                match self.client.get_file(&mapping.ipfs_cid).await {
                    Ok(data) => {
                        // Cache the object if caching is enabled
                        if self.cache_enabled {
                            // Convert object type string back to enum
                            let object_type = match mapping.object_type.as_str() {
                                "blob" => ObjectType::Blob,
                                "tree" => ObjectType::Tree,
                                "commit" => ObjectType::Commit,
                                "tag" => ObjectType::Tag,
                                _ => return Err(GitError::IpfsError(format!("Invalid object type: {}", mapping.object_type)))
                            };
                            
                            if let Err(e) = self.store_in_cache(id, object_type, &data).await {
                                log::warn!("Failed to cache object: {}", e);
                            }
                        }
                        
                        // Update cache miss stats
                        {
                            let mut stats = self.stats.write().await;
                            stats.misses += 1;
                        }
                        
                        // Convert object type string back to enum
                        let object_type = match mapping.object_type.as_str() {
                            "blob" => ObjectType::Blob,
                            "tree" => ObjectType::Tree,
                            "commit" => ObjectType::Commit,
                            "tag" => ObjectType::Tag,
                            _ => return Err(GitError::IpfsError(format!("Invalid object type: {}", mapping.object_type)))
                        };
                        
                        Ok((object_type, data))
                    },
                    Err(e) => Err(GitError::IpfsError(format!("Failed to get object from IPFS: {}", e)))
                }
            },
            None => Err(GitError::ObjectStorage(format!("Object not found: {}", id)))
        }
    }
    
    async fn store_object(&self, object_type: ObjectType, data: &[u8]) -> Result<ObjectId> {
        // Check if background uploads are enabled and this is a large blob
        if self.settings.use_background_uploads && 
           object_type == ObjectType::Blob && 
           data.len() > self.settings.chunking_threshold {
            log::debug!("Using background upload for large blob ({} bytes)", data.len());
            return self.submit_background_upload(object_type, Bytes::copy_from_slice(data)).await;
        }
        
        // Otherwise store directly
        self.store_object_internal(object_type, data).await
    }
    
    async fn has_object(&self, id: &ObjectId) -> bool {
        // Check ongoing background uploads
        let is_uploading = {
            let tasks = self.background_tasks.lock().await;
            tasks.contains_key(&id.to_string())
        };
        
        if is_uploading {
            return true;
        }
        
        // Check in memory mappings
        let has_mapping = {
            let mappings = self.mappings.read().await;
            mappings.contains_key(&id.to_string())
        };
        
        if has_mapping {
            return true;
        }
        
        // Check local cache if enabled
        if self.cache_enabled && self.is_in_cache(id) {
            return true;
        }
        
        false
    }
    
    async fn get_object_cid(&self, id: &ObjectId) -> Result<String> {
        // Check if we have a mapping for this object
        let mapping = {
            let mappings = self.mappings.read().await;
            mappings.get(&id.to_string()).cloned()
        };
        
        match mapping {
            Some(mapping) => Ok(mapping.ipfs_cid),
            None => Err(GitError::ObjectStorage(format!("Object not found: {}", id)))
        }
    }
    
    fn get_stats(&self) -> CacheStats {
        // We can't use async here because the trait method isn't async
        // So we'll just return a clone of the current stats or default if we can't get the lock
        tokio::task::block_in_place(|| {
            match self.stats.try_read() {
                Ok(stats) => *stats,
                Err(_) => CacheStats::default(),
            }
        })
    }

    async fn store_objects_batch(&self, objects: Vec<(ObjectType, Bytes)>) -> Result<Vec<ObjectId>> {
        log::debug!("Batch storing {} objects", objects.len());
        
        let mut object_ids = Vec::with_capacity(objects.len());
        
        // For small batches, process sequentially
        if objects.len() < 5 {
            for (object_type, data) in objects {
                let id = self.store_object(object_type, &data).await?;
                object_ids.push(id);
            }
            return Ok(object_ids);
        }
        
        // For larger batches, use parallel processing with throttling
        let semaphore = tokio::sync::Semaphore::new(4); // Limit concurrent uploads
        let mut handles = Vec::with_capacity(objects.len());
        
        for (object_type, data) in objects {
            let storage_clone = self.clone();
            let permit = semaphore.acquire().await.unwrap();
            
            let handle = tokio::spawn(async move {
                let result = storage_clone.store_object(object_type, &data).await;
                drop(permit); // Release the permit when done
                result
            });
            
            handles.push(handle);
        }
        
        // Collect results
        for handle in handles {
            match handle.await {
                Ok(result) => {
                    match result {
                        Ok(id) => object_ids.push(id),
                        Err(e) => return Err(e),
                    }
                },
                Err(e) => {
                    return Err(GitError::IpfsError(format!("Failed to join task: {}", e)));
                }
            }
        }
        
        Ok(object_ids)
    }

    async fn get_objects_batch(&self, ids: &[ObjectId]) -> Result<Vec<(ObjectId, ObjectType, Bytes)>> {
        log::debug!("Batch retrieving {} objects", ids.len());
        
        let mut objects = Vec::with_capacity(ids.len());
        
        // For small batches, process sequentially
        if ids.len() < 5 {
            for id in ids {
                let (object_type, data) = self.get_object(id).await?;
                objects.push((id.clone(), object_type, data));
            }
            return Ok(objects);
        }
        
        // For larger batches, use parallel processing with throttling
        let semaphore = tokio::sync::Semaphore::new(4); // Limit concurrent downloads
        let mut handles = Vec::with_capacity(ids.len());
        
        for id in ids {
            let storage_clone = self.clone();
            let id_clone = id.clone();
            let permit = semaphore.acquire().await.unwrap();
            
            let handle = tokio::spawn(async move {
                let result = storage_clone.get_object(&id_clone).await
                    .map(|(object_type, data)| (id_clone, object_type, data));
                drop(permit); // Release the permit when done
                result
            });
            
            handles.push(handle);
        }
        
        // Collect results
        for handle in handles {
            match handle.await {
                Ok(result) => {
                    match result {
                        Ok(object) => objects.push(object),
                        Err(e) => return Err(e),
                    }
                },
                Err(e) => {
                    return Err(GitError::IpfsError(format!("Failed to join task: {}", e)));
                }
            }
        }
        
        Ok(objects)
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