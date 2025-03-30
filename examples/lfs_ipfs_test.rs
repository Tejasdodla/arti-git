use arti_git::core::Result;
use arti_git::ipfs::{IpfsClient, IpfsConfig};
use arti_git::lfs::{LfsObjectId, LfsPointer, LfsStorage};
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Testing Git LFS + IPFS integration");

    // Set up test paths
    let base_dir = PathBuf::from("./lfs-objects");
    let test_file = PathBuf::from("./test-file.txt");
    let pointer_file = PathBuf::from("./test-file.pointer");
    let retrieved_file = PathBuf::from("./retrieved-test-file.txt");

    // Create test data
    let test_content = b"This is a test file for Git LFS + IPFS integration.";
    println!("Creating test file...");
    tokio::fs::write(&test_file, test_content).await?;

    // Initialize IPFS client
    println!("Initializing IPFS client...");
    let ipfs_config = IpfsConfig {
        api_url: "http://localhost:5001".to_string(),
        gateway_url: "http://localhost:8080".to_string(),
        auto_pin: true,
    };
    let ipfs_client = Arc::new(IpfsClient::new(ipfs_config).await?);

    // Initialize LFS storage with IPFS integration
    println!("Initializing LFS storage with IPFS...");
    let lfs_storage = LfsStorage::with_ipfs(&base_dir, ipfs_client.clone(), true)?;

    // Create a test object ID (normally calculated from the content)
    let object_id = format!("sha256:{}", hex::encode(sha1::Sha1::digest(test_content)));
    let lfs_id = LfsObjectId::new(&object_id);

    // Store the object using LFS (which will also store it in IPFS)
    println!("Storing object in LFS and IPFS...");
    lfs_storage.store_object(&lfs_id, test_content).await?;

    // Get the IPFS CID for the object
    let cid = lfs_storage.get_ipfs_cid(&lfs_id);
    println!("Object stored with IPFS CID: {:?}", cid);

    // Create a pointer file
    let mut pointer = LfsPointer::new(&object_id, test_content.len() as u64);
    if let Some(cid) = cid {
        pointer.set_ipfs_cid(&cid);
    }

    // Write the pointer file
    println!("Creating LFS pointer file...");
    pointer.write_to_file(&pointer_file).await?;

    // Read the pointer file back
    println!("Reading LFS pointer file...");
    let read_pointer = LfsPointer::from_file(&pointer_file).await?;
    println!("Pointer contents: {}", read_pointer);

    // Retrieve the object from storage
    println!("Retrieving object from storage...");
    let retrieved_content = lfs_storage.get_object_bytes(&lfs_id).await?;

    // Write the retrieved content to a new file
    println!("Writing retrieved content to file...");
    tokio::fs::write(&retrieved_file, retrieved_content).await?;

    // Verify the content matches
    let retrieved_content = tokio::fs::read(&retrieved_file).await?;
    assert_eq!(retrieved_content, test_content);
    println!("Content verification successful!");

    // Clean up
    println!("Cleaning up...");
    tokio::fs::remove_file(&test_file).await?;
    tokio::fs::remove_file(&pointer_file).await?;
    tokio::fs::remove_file(&retrieved_file).await?;

    println!("Test completed successfully!");
    Ok(())
}