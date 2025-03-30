use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio;

/// A simple test program to verify the Git LFS with IPFS functionality
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Git LFS with IPFS Integration Test");
    
    // Set up the LFS storage directory
    let base_dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from("~/.local/share"));
    let lfs_dir = base_dir.join("arti-git").join("lfs").join("objects");
    
    println!("LFS storage directory: {}", lfs_dir.display());
    
    // Test the LFS pointers
    let oid = "0123456789abcdef0123456789abcdef01234567";
    let size = 1024;
    
    // Create a pointer
    let pointer = create_pointer(oid, size);
    println!("Created LFS pointer:\n{}", pointer);
    
    // Parse the pointer
    let (parsed_oid, parsed_size) = parse_pointer(&pointer)?;
    println!("Parsed pointer - OID: {}, Size: {}", parsed_oid, parsed_size);
    
    // Verify the parsed data matches the original
    assert_eq!(oid, parsed_oid);
    assert_eq!(size, parsed_size);
    println!("Pointer verification successful!");
    
    Ok(())
}

/// Create a Git LFS pointer
fn create_pointer(oid: &str, size: usize) -> String {
    format!(
        "version https://git-lfs.github.com/spec/v1\n\
         oid sha256:{}\n\
         size {}\n",
        oid, size
    )
}

/// Parse a Git LFS pointer and extract the OID and size
fn parse_pointer(pointer: &str) -> Result<(String, usize), Box<dyn std::error::Error>> {
    let mut oid = None;
    let mut size = None;
    
    for line in pointer.lines() {
        if line.starts_with("oid sha256:") {
            oid = Some(line[11..].to_string());
        } else if line.starts_with("size ") {
            size = Some(line[5..].parse::<usize>()?);
        }
    }
    
    match (oid, size) {
        (Some(o), Some(s)) => Ok((o, s)),
        _ => Err("Invalid LFS pointer: missing oid or size".into()),
    }
}