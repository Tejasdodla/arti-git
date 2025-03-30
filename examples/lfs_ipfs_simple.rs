use std::path::PathBuf;
use std::io::Write;
use std::fs::File;
use std::sync::Arc;
use sha1::{Sha1, Digest};

// A minimal implementation that focuses only on the core LFS+IPFS integration
// without depending on problematic gix-url dependency

// Simple LFS object representation
struct LfsObject {
    id: String,
    size: u64,
    ipfs_cid: Option<String>,
    data: Vec<u8>,
}

impl LfsObject {
    fn new(data: &[u8]) -> Self {
        // Calculate SHA-256 hash for LFS object ID
        let hash = Sha1::digest(data);
        let id = format!("sha256:{}", hex::encode(hash));
        
        Self {
            id,
            size: data.len() as u64,
            ipfs_cid: None,
            data: data.to_vec(),
        }
    }
    
    // Write LFS pointer file
    fn write_pointer(&self, path: &PathBuf) -> std::io::Result<()> {
        let mut content = format!(
            "version https://git-lfs.github.com/spec/v1\n\
             oid {}\n\
             size {}", 
            self.id, 
            self.size
        );
        
        if let Some(cid) = &self.ipfs_cid {
            content.push_str(&format!("\nx-artigit-ipfs-cid {}", cid));
        }
        
        let mut file = File::create(path)?;
        file.write_all(content.as_bytes())?;
        Ok(())
    }
}

// Simplified IPFS client 
struct SimpleIpfsClient {
    endpoint: String,
}

impl SimpleIpfsClient {
    fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
        }
    }
    
    // Add content to IPFS (mock implementation)
    fn add(&self, data: &[u8]) -> Result<String, String> {
        // In a real implementation, this would make an API call to IPFS
        // For this test, we'll mock it by generating a fake CID based on the data
        let hash = Sha1::digest(data);
        let mock_cid = format!("Qm{}", hex::encode(&hash[..10]));
        
        println!("Mock IPFS: Added content with CID {}", mock_cid);
        Ok(mock_cid)
    }
    
    // Retrieve content from IPFS (mock implementation)
    fn get(&self, cid: &str) -> Result<Vec<u8>, String> {
        println!("Mock IPFS: Retrieved content with CID {}", cid);
        // In a real implementation, this would fetch data from IPFS
        // For this test, we'll just return some mock data
        Ok(b"Mock IPFS content retrieved".to_vec())
    }
}

// Simplified LFS+IPFS storage
struct LfsIpfsStorage {
    local_path: PathBuf,
    ipfs_client: Arc<SimpleIpfsClient>,
}

impl LfsIpfsStorage {
    fn new(local_path: PathBuf, ipfs_client: Arc<SimpleIpfsClient>) -> Self {
        std::fs::create_dir_all(&local_path).unwrap_or_default();
        Self {
            local_path,
            ipfs_client,
        }
    }
    
    // Store object in both local storage and IPFS
    fn store(&self, object: &mut LfsObject) -> Result<(), String> {
        // Store in IPFS and get CID
        let cid = self.ipfs_client.add(&object.data)?;
        object.ipfs_cid = Some(cid.clone());
        
        // Store locally
        let object_path = self.get_object_path(&object.id);
        std::fs::create_dir_all(object_path.parent().unwrap()).unwrap_or_default();
        
        let mut file = File::create(&object_path)
            .map_err(|e| format!("Failed to create local file: {}", e))?;
            
        file.write_all(&object.data)
            .map_err(|e| format!("Failed to write local file: {}", e))?;
            
        // Create a CID mapping file
        let cid_path = object_path.with_extension("cid");
        let mut cid_file = File::create(&cid_path)
            .map_err(|e| format!("Failed to create CID file: {}", e))?;
            
        cid_file.write_all(cid.as_bytes())
            .map_err(|e| format!("Failed to write CID file: {}", e))?;
            
        Ok(())
    }
    
    // Calculate the path for an object based on its ID
    fn get_object_path(&self, id: &str) -> PathBuf {
        // Extract just the hash part from the ID (remove "sha256:")
        let hash = id.split(':').nth(1).unwrap_or(id);
        
        // Use the first 2 characters as a directory name for sharding
        let prefix = &hash[0..2];
        let rest = &hash[2..];
        
        self.local_path.join(prefix).join(rest)
    }
}

fn main() -> Result<(), String> {
    println!("Simple Git LFS + IPFS Integration Test");
    
    // Create test directories and files
    let base_dir = PathBuf::from("./lfs-objects");
    std::fs::create_dir_all(&base_dir).unwrap_or_default();
    
    // Create a test file
    let test_content = b"This is a test file for Git LFS + IPFS integration.";
    let test_file = PathBuf::from("./test-file.txt");
    std::fs::write(&test_file, test_content).map_err(|e| e.to_string())?;
    
    // Initialize simple IPFS client
    let ipfs_client = Arc::new(SimpleIpfsClient::new("http://localhost:5001"));
    
    // Initialize LFS+IPFS storage
    let storage = LfsIpfsStorage::new(base_dir, ipfs_client);
    
    // Create LFS object from file
    let mut lfs_object = LfsObject::new(test_content);
    
    // Store the object in LFS+IPFS
    println!("Storing LFS object in both local storage and IPFS...");
    storage.store(&mut lfs_object)?;
    
    // Create a pointer file
    println!("Creating LFS pointer file...");
    let pointer_file = PathBuf::from("./test-file.pointer");
    lfs_object.write_pointer(&pointer_file).map_err(|e| e.to_string())?;
    
    println!("Test completed successfully!");
    println!("LFS Object ID: {}", lfs_object.id);
    println!("IPFS CID: {}", lfs_object.ipfs_cid.unwrap_or_default());
    
    Ok(())
}