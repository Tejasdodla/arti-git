use std::io;
use std::path::{Path, PathBuf};
use std::collections::HashMap;

use bytes::{Bytes, BytesMut, Buf, BufMut};
use gix::{Repository, oid};
use gix_hash::ObjectId;
use gix_packetline::{self as pkt, PacketLine, WriteMode};
use tokio::io::{AsyncRead, AsyncWrite, AsyncReadExt, AsyncWriteExt};

use crate::core::GitError;

/// A parsed Git command
pub struct GitCommand {
    /// The Git service name (git-upload-pack or git-receive-pack)
    pub service: String,
    
    /// The repository path (relative to the service root)
    pub repo_path: PathBuf,
    
    /// Additional parameters from the request
    pub params: HashMap<String, String>,
}

/// Parse a Git smart protocol command from a stream
pub async fn parse_git_command<S>(stream: &mut S) -> io::Result<GitCommand>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buf = [0u8; 4096];
    let bytes_read = stream.read(&mut buf).await?;
    
    if bytes_read == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Empty request"
        ));
    }
    
    let request = std::str::from_utf8(&buf[..bytes_read])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid UTF-8 in request"))?;
    
    // Git commands are in the format: git-service-name path\0host=hostname\0
    let mut parts = request.split('\0');
    let first_part = parts.next().ok_or_else(|| io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid Git protocol request format"
    ))?;
    
    // Parse the first part to get service and path
    let mut command_parts = first_part.split_whitespace();
    let service = command_parts.next().ok_or_else(|| io::Error::new(
        io::ErrorKind::InvalidData,
        "Missing Git service name"
    ))?.to_string();
    
    let repo_path = command_parts.next()
        .unwrap_or("/")
        .trim_start_matches('/')
        .to_string();
    
    // Parse additional parameters (host, etc.)
    let mut params = HashMap::new();
    for param in parts {
        if let Some(pos) = param.find('=') {
            let key = &param[..pos];
            let value = &param[pos + 1..];
            params.insert(key.to_string(), value.to_string());
        }
    }
    
    Ok(GitCommand {
        service,
        repo_path: PathBuf::from(repo_path),
        params,
    })
}

/// Send Git references advertisement to client
pub async fn send_refs_advertisement<S>(
    stream: &mut S, 
    repo: &Repository,
    service: &str,
    advertise_capabilities: bool,
) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    // First line is special if we're advertising capabilities
    let mut capabilities = String::new();
    
    if advertise_capabilities {
        capabilities.push_str(" report-status delete-refs side-band-64k quiet");
        
        if service == "git-receive-pack" {
            capabilities.push_str(" report-status-v2 push-options");
        } else if service == "git-upload-pack" {
            capabilities.push_str(" multi_ack thin-pack ofs-delta shallow no-progress include-tag");
        }
    }
    
    // Get all references
    let refs = repo.references()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to get refs: {}", e)))?;
    
    let refs_list: Vec<_> = refs.all()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to list refs: {}", e)))?
        .filter_map(Result::ok)
        .collect();
    
    // Send first reference with capabilities
    if let Some(first_ref) = refs_list.first() {
        let oid = first_ref.id().to_hex().to_string();
        let name = first_ref.name().as_bstr();
        
        let mut first_line = format!("{} {}", oid, name);
        if advertise_capabilities {
            first_line.push('\0');
            first_line.push_str(&capabilities);
        }
        
        // Send the packet line
        let packet = format!("{:04x}{}\n", first_line.len() + 4, first_line);
        stream.write_all(packet.as_bytes()).await?;
    } else {
        // If no refs, send capabilities with a null OID
        let null_oid = "0000000000000000000000000000000000000000";
        let first_line = format!("{} capabilities^{}\0{}", null_oid, capabilities);
        
        // Send the packet line
        let packet = format!("{:04x}{}\n", first_line.len() + 4, first_line);
        stream.write_all(packet.as_bytes()).await?;
    }
    
    // Send the rest of the references
    for git_ref in refs_list.iter().skip(1) {
        let oid = git_ref.id().to_hex().to_string();
        let name = git_ref.name().as_bstr();
        
        let line = format!("{} {}", oid, name);
        let packet = format!("{:04x}{}\n", line.len() + 4, line);
        stream.write_all(packet.as_bytes()).await?;
    }
    
    // Send a flush packet
    stream.write_all(b"0000").await?;
    
    Ok(())
}

/// Process Git upload-pack (fetch/clone) negotiation
pub async fn process_wants<S>(
    stream: &mut S,
    repo: &Repository
) -> io::Result<Vec<ObjectId>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut wanted_objects = Vec::new();
    let mut have_objects = Vec::new();
    let mut buf = [0u8; 4096];
    
    // Read the client's wants
    loop {
        let n = stream.read(&mut buf).await?;
        if n < 4 {
            break;
        }
        
        let line = std::str::from_utf8(&buf[..n])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid UTF-8 in request"))?;
        
        if line.starts_with("0000") {
            // Flush packet - end of wants
            break;
        } else if line.starts_with("want ") {
            // Parse the wanted object
            let oid_hex = line[5..45].to_string();
            match ObjectId::from_hex(oid_hex.as_bytes()) {
                Ok(oid) => wanted_objects.push(oid),
                Err(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid object ID")),
            }
        } else if line.starts_with("have ") {
            // Parse the client's have
            let oid_hex = line[5..45].to_string();
            match ObjectId::from_hex(oid_hex.as_bytes()) {
                Ok(oid) => have_objects.push(oid),
                Err(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid object ID")),
            }
        }
    }
    
    // Send acknowledgement
    if !have_objects.is_empty() {
        stream.write_all(b"0008NAK\n").await?;
    }
    
    Ok(wanted_objects)
}

/// Send a packfile containing the requested objects
pub async fn send_packfile<S>(
    stream: &mut S,
    repo: &Repository, 
    wanted_objects: &[ObjectId]
) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    if wanted_objects.is_empty() {
        // No objects requested, send an empty flush packet
        stream.write_all(b"0000").await?;
        return Ok(());
    }

    println!("Sending packfile with {} requested objects", wanted_objects.len());

    // We use side-band-64k protocol for better error reporting
    // This means we send our packfile data with a 1-byte channel prefix
    // Channel 1: packfile data
    // Channel 2: progress messages
    // Channel 3: error messages

    // Send packfile header indicator
    stream.write_all(b"0008\x01PACK").await?;

    // Create a packfile writer
    let mut pack_builder = gix_pack::data::output::Builder::default();

    // Add objects to the packfile
    let mut object_count = 0;
    for want in wanted_objects {
        // Add the object and all its dependencies to the packfile
        let object = repo.find_object(*want)
            .map_err(|e| io::Error::new(io::ErrorKind::NotFound, format!("Object not found: {}", e)))?;

        // Walk the object graph to include all needed objects
        let mut traversal = repo.objects.traverse([object.id])?
            .with_deepen(true)  // Include all tree entries for tree objects
            .with_objects(true)  // Include all reachable objects
            .with_tags(true);   // Include tag objects

        while let Some(obj) = traversal.next() {
            let obj = obj.map_err(|e| io::Error::new(
                io::ErrorKind::Other, 
                format!("Failed to traverse object: {}", e)
            ))?;

            // Progress reporting on a separate channel
            if object_count % 100 == 0 {
                let progress_msg = format!("Processing object {}/{}", object_count, traversal.total_objects());
                let packet = format!("{:04x}\x02{}", progress_msg.len() + 5, progress_msg);
                stream.write_all(packet.as_bytes()).await?;
            }

            // Add object to the pack
            pack_builder.add_object(obj.data.into(), obj.kind)?;
            object_count += 1;
        }
    }

    // Send progress message
    let msg = format!("Preparing to send {} objects", object_count);
    let packet = format!("{:04x}\x02{}", msg.len() + 5, msg);
    stream.write_all(packet.as_bytes()).await?;

    // Finalize and send the packfile in chunks
    let pack_data = pack_builder.finish();
    
    // Send the packfile data in chunks that fit into the side-band-64k protocol (max 65519 bytes per packet)
    const MAX_CHUNK_SIZE: usize = 65515; // 65519 - 4 bytes for length prefix - 1 byte for channel
    let mut offset = 0;

    while offset < pack_data.len() {
        let chunk_size = std::cmp::min(MAX_CHUNK_SIZE, pack_data.len() - offset);
        let chunk = &pack_data[offset..offset + chunk_size];

        // Format packet: 4-digit hex length + 1-byte channel + data
        let packet_size = chunk_size + 5; // +5 for 4 length bytes and 1 channel byte
        let header = format!("{:04x}\x01", packet_size);
        
        // Write packet header and chunk data
        stream.write_all(header.as_bytes()).await?;
        stream.write_all(chunk).await?;
        
        offset += chunk_size;
        
        // Send progress update occasionally
        if offset % (1024 * 1024) == 0 {
            let progress = format!("Sent {}/{} bytes", offset, pack_data.len());
            let progress_packet = format!("{:04x}\x02{}", progress.len() + 5, progress);
            stream.write_all(progress_packet.as_bytes()).await?;
        }
    }

    // Send completion message
    let complete_msg = format!("Pack transfer complete. {} objects sent.", object_count);
    let complete_packet = format!("{:04x}\x02{}", complete_msg.len() + 5, complete_msg);
    stream.write_all(complete_packet.as_bytes()).await?;
    
    // Send flush packet to indicate end of packfile
    stream.write_all(b"0000").await?;
    
    println!("Packfile sent successfully: {} objects, {} bytes", object_count, pack_data.len());

    Ok(())
}

/// Process Git receive-pack (push) requests
pub async fn receive_packfile<S>(
    stream: &mut S, 
    repo: &Repository
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    println!("Receiving packfile from client...");
    
    // First, read the client's reference updates
    let mut ref_updates = HashMap::new();
    let mut line_buf = Vec::new();
    let mut length_buf = [0u8; 4];
    
    // Read reference update commands
    loop {
        // Read packet length
        stream.read_exact(&mut length_buf).await?;
        let length_str = std::str::from_utf8(&length_buf)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid packet length encoding"))?;
            
        // Check for flush packet
        if length_str == "0000" {
            break;
        }
        
        // Parse length
        let length = u16::from_str_radix(length_str, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid packet length"))?;
            
        if length < 4 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid packet length"));
        }
        
        // Read packet data
        let data_length = length as usize - 4; // Subtract the 4 bytes of the length header
        line_buf.resize(data_length, 0);
        stream.read_exact(&mut line_buf).await?;
        
        // Parse reference update command
        let line = std::str::from_utf8(&line_buf)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid UTF-8 in packet"))?;
            
        // Reference update format: <old-oid> <new-oid> <ref-name>
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let old_oid_str = parts[0];
            let new_oid_str = parts[1];
            let ref_name = parts[2];
            
            // Parse object IDs
            let old_oid = if old_oid_str == "0000000000000000000000000000000000000000" {
                None
            } else {
                match ObjectId::from_hex(old_oid_str.as_bytes()) {
                    Ok(oid) => Some(oid),
                    Err(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid old object ID")),
                }
            };
            
            let new_oid = if new_oid_str == "0000000000000000000000000000000000000000" {
                // Deletion
                None
            } else {
                match ObjectId::from_hex(new_oid_str.as_bytes()) {
                    Ok(oid) => Some(oid),
                    Err(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid new object ID")),
                }
            };
            
            ref_updates.insert(ref_name.to_string(), (old_oid, new_oid));
        }
    }
    
    // Report unpack success first
    stream.write_all(b"000eunpack ok\n").await?;
    
    // Apply the reference updates
    let mut results = Vec::new();
    
    for (ref_name, (old_oid, new_oid)) in ref_updates {
        let result = match (old_oid, new_oid) {
            (_, None) => {
                // Delete reference
                match repo.references.delete(&ref_name) {
                    Ok(_) => {
                        println!("Deleted reference: {}", ref_name);
                        format!("ok {}", ref_name)
                    },
                    Err(e) => {
                        eprintln!("Failed to delete reference {}: {}", ref_name, e);
                        format!("ng {} deletion failed", ref_name)
                    }
                }
            },
            (None, Some(new_id)) => {
                // Create new reference
                match repo.references.create(&ref_name, new_id, false, &format!("push: created {}", ref_name)) {
                    Ok(_) => {
                        println!("Created reference: {} -> {}", ref_name, new_id);
                        format!("ok {}", ref_name)
                    },
                    Err(e) => {
                        eprintln!("Failed to create reference {}: {}", ref_name, e);
                        format!("ng {} creation failed", ref_name)
                    }
                }
            },
            (Some(old_id), Some(new_id)) => {
                // Update existing reference
                // First verify that the old value matches what we expect
                match repo.references.find(&ref_name) {
                    Ok(existing_ref) => {
                        if existing_ref.id() != old_id {
                            eprintln!("Reference update failed: {} expected {}, found {}", 
                                      ref_name, old_id, existing_ref.id());
                            format!("ng {} expected old value {} was {}", 
                                   ref_name, old_id, existing_ref.id())
                        } else {
                            // Update the reference
                            match repo.references.create_matching(&ref_name, new_id, false, old_id, 
                                                                &format!("push: update {}", ref_name)) {
                                Ok(_) => {
                                    println!("Updated reference: {} {} -> {}", ref_name, old_id, new_id);
                                    format!("ok {}", ref_name)
                                },
                                Err(e) => {
                                    eprintln!("Failed to update reference {}: {}", ref_name, e);
                                    format!("ng {} update failed", ref_name)
                                }
                            }
                        }
                    },
                    Err(e) => {
                        eprintln!("Failed to find reference {}: {}", ref_name, e);
                        format!("ng {} not found", ref_name)
                    }
                }
            }
        };
        
        results.push(result);
    }
    
    // Send all the reference update results
    for result in results {
        let packet = format!("{:04x}{}\n", result.len() + 4, result);
        stream.write_all(packet.as_bytes()).await?;
    }
    
    // Send flush packet to indicate end of reference updates
    stream.write_all(b"0000").await?;
    
    println!("Repository references updated successfully");
    Ok(())
}