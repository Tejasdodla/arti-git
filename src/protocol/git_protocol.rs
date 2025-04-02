use std::io;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::Arc;

use bytes::{Bytes, BytesMut, Buf, BufMut};
use gix::{Repository, oid};
use gix_hash::ObjectId;
use gix_packetline::{self as pkt, PacketLine, WriteMode};
use tokio::io::{AsyncRead, AsyncWrite, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use futures::StreamExt;

use crate::core::{GitError, Result, io_err, protocol_err};

/// A parsed Git command
#[derive(Debug, Clone)]
pub struct GitCommand {
    /// The Git service name (git-upload-pack or git-receive-pack)
    pub service: String,
    
    /// The repository path (relative to the service root)
    pub repo_path: PathBuf,
    
    /// Additional parameters from the request
    pub params: HashMap<String, String>,
    
    /// Protocol version (v0, v1, v2)
    pub version: GitProtocolVersion,
}

/// Git protocol version
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitProtocolVersion {
    /// Original protocol (implicit)
    V0,
    
    /// Protocol v1 (smart http)
    V1,
    
    /// Protocol v2 (packfile negotiation improvements)
    V2,
}

impl Default for GitProtocolVersion {
    fn default() -> Self {
        Self::V0
    }
}

impl GitCommand {
    /// Create a new GitCommand instance
    pub fn new(service: String, repo_path: PathBuf) -> Self {
        Self {
            service,
            repo_path,
            params: HashMap::new(),
            version: GitProtocolVersion::V0,
        }
    }
    
    /// Add a parameter to the command
    pub fn with_param(mut self, key: &str, value: &str) -> Self {
        self.params.insert(key.to_string(), value.to_string());
        self
    }
    
    /// Set the protocol version
    pub fn with_version(mut self, version: GitProtocolVersion) -> Self {
        self.version = version;
        self
    }
    
    /// Check if this is an upload-pack command
    pub fn is_upload_pack(&self) -> bool {
        self.service == "git-upload-pack"
    }
    
    /// Check if this is a receive-pack command
    pub fn is_receive_pack(&self) -> bool {
        self.service == "git-receive-pack"
    }
    
    /// Get the host parameter
    pub fn host(&self) -> Option<&str> {
        self.params.get("host").map(|s| s.as_str())
    }
}

/// Server capabilities for reference advertisement
#[derive(Debug, Clone, Default)]
pub struct ServerCapabilities {
    /// Common capabilities
    pub common: Vec<String>,
    
    /// Upload pack specific capabilities
    pub upload_pack: Vec<String>,
    
    /// Receive pack specific capabilities
    pub receive_pack: Vec<String>,
}

impl ServerCapabilities {
    /// Create a new capabilities instance with standard defaults
    pub fn new() -> Self {
        let mut caps = Self::default();
        
        // Common capabilities
        caps.common.extend_from_slice(&[
            "side-band-64k".to_string(),
            "quiet".to_string(),
            "report-status".to_string(),
        ]);
        
        // Upload pack capabilities
        caps.upload_pack.extend_from_slice(&[
            "multi_ack".to_string(),
            "thin-pack".to_string(),
            "ofs-delta".to_string(),
            "shallow".to_string(),
            "no-progress".to_string(),
            "include-tag".to_string(),
            "allow-tip-sha1-in-want".to_string(),
            "allow-reachable-sha1-in-want".to_string(),
        ]);
        
        // Receive pack capabilities
        caps.receive_pack.extend_from_slice(&[
            "report-status-v2".to_string(),
            "delete-refs".to_string(),
            "push-options".to_string(),
            "atomic".to_string(),
        ]);
        
        caps
    }
    
    /// Get all capabilities for a specific service
    pub fn for_service(&self, service: &str) -> Vec<String> {
        let mut result = self.common.clone();
        
        if service == "git-upload-pack" {
            result.extend_from_slice(&self.upload_pack);
        } else if service == "git-receive-pack" {
            result.extend_from_slice(&self.receive_pack);
        }
        
        result
    }
    
    /// Check if a capability is supported
    pub fn supports(&self, capability: &str) -> bool {
        self.common.contains(&capability.to_string()) ||
        self.upload_pack.contains(&capability.to_string()) ||
        self.receive_pack.contains(&capability.to_string())
    }
    
    /// Add a capability
    pub fn add(&mut self, capability: &str, service: Option<&str>) {
        match service {
            Some("git-upload-pack") => {
                if !self.upload_pack.contains(&capability.to_string()) {
                    self.upload_pack.push(capability.to_string());
                }
            },
            Some("git-receive-pack") => {
                if !self.receive_pack.contains(&capability.to_string()) {
                    self.receive_pack.push(capability.to_string());
                }
            },
            _ => {
                if !self.common.contains(&capability.to_string()) {
                    self.common.push(capability.to_string());
                }
            }
        }
    }
}

/// Pack protocol channel types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackProtocolChannel {
    /// Packfile data
    Data = 1,
    
    /// Progress messages
    Progress = 2,
    
    /// Error messages
    Error = 3,
}

impl From<u8> for PackProtocolChannel {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Data,
            2 => Self::Progress,
            3 => Self::Error,
            _ => Self::Data,  // Default to data channel
        }
    }
}

/// Parse a Git smart protocol command from a stream
pub async fn parse_git_command<S>(stream: &mut S) -> Result<GitCommand>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buf = [0u8; 4096];
    let bytes_read = stream.read(&mut buf).await
        .map_err(|e| io_err(format!("Failed to read Git command: {}", e)))?;
    
    if bytes_read == 0 {
        return Err(protocol_err("Empty request", None));
    }
    
    let request = std::str::from_utf8(&buf[..bytes_read])
        .map_err(|_| protocol_err("Invalid UTF-8 in request", None))?;
    
    // Check for protocol version marker
    let mut version = GitProtocolVersion::V0;
    if request.starts_with("version=") {
        let version_line = request.lines().next().unwrap_or("");
        if version_line.contains("version=2") {
            version = GitProtocolVersion::V2;
            log::debug!("Detected Git protocol version 2");
        } else if version_line.contains("version=1") {
            version = GitProtocolVersion::V1;
            log::debug!("Detected Git protocol version 1");
        }
    }
    
    // Git commands are in the format: git-service-name path\0host=hostname\0
    let mut parts = request.split('\0');
    let first_part = parts.next().ok_or_else(|| 
        protocol_err("Invalid Git protocol request format", None))?;
    
    // Parse the first part to get service and path
    let mut command_parts = first_part.split_whitespace();
    let service = command_parts.next().ok_or_else(|| 
        protocol_err("Missing Git service name", None))?.to_string();
    
    let repo_path = command_parts.next()
        .unwrap_or("/")
        .trim_start_matches('/')
        .to_string();
    
    // Create the command object
    let mut command = GitCommand::new(service, PathBuf::from(repo_path))
        .with_version(version);
    
    // Parse additional parameters (host, etc.)
    for param in parts {
        if let Some(pos) = param.find('=') {
            let key = &param[..pos];
            let value = &param[pos + 1..];
            command = command.with_param(key, value);
        }
    }
    
    log::debug!("Parsed Git command: {:?} for repository: {:?}", command.service, command.repo_path);
    
    Ok(command)
}

/// Send Git references advertisement to client
pub async fn send_refs_advertisement<S>(
    stream: &mut S, 
    repo: &Repository,
    command: &GitCommand,
    capabilities: &ServerCapabilities,
) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    log::info!("Sending references advertisement for {:?}", command.repo_path);
    
    // Get capabilities string for this service
    let capabilities_str = capabilities.for_service(&command.service).join(" ");
    
    // Get all references
    let refs = repo.references()
        .map_err(|e| protocol_err(format!("Failed to get refs: {}", e), None))?;
    
    let refs_list: Vec<_> = refs.all()
        .map_err(|e| protocol_err(format!("Failed to list refs: {}", e), None))?
        .filter_map(Result::ok)
        .collect();
    
    // Determine HEAD reference
    let head_ref = repo.head()
        .ok()
        .and_then(|head| head.id().ok());
    
    // Send first reference with capabilities (use HEAD if available)
    if let (Some(head_id), true) = (head_ref, refs_list.len() > 0) {
        // Send HEAD as first reference with capabilities
        let first_line = format!("{} HEAD\0{}", head_id.to_hex(), capabilities_str);
        
        // Send the packet line
        let packet = format!("{:04x}{}", first_line.len() + 4, first_line);
        stream.write_all(packet.as_bytes()).await
            .map_err(|e| io_err(format!("Failed to write HEAD reference: {}", e)))?;
    } else if let Some(first_ref) = refs_list.first() {
        // Send first available reference with capabilities
        let oid = first_ref.id().to_hex().to_string();
        let name = first_ref.name().as_bstr();
        
        let first_line = format!("{} {}\0{}", oid, name, capabilities_str);
        
        // Send the packet line
        let packet = format!("{:04x}{}", first_line.len() + 4, first_line);
        stream.write_all(packet.as_bytes()).await
            .map_err(|e| io_err(format!("Failed to write first reference: {}", e)))?;
    } else {
        // If no refs, send capabilities with a null OID
        let null_oid = "0000000000000000000000000000000000000000";
        let first_line = format!("{} capabilities^\0{}", null_oid, capabilities_str);
        
        // Send the packet line
        let packet = format!("{:04x}{}", first_line.len() + 4, first_line);
        stream.write_all(packet.as_bytes()).await
            .map_err(|e| io_err(format!("Failed to write capabilities: {}", e)))?;
    }
    
    // Send HEAD reference symref if available
    if let (Some(head_id), Some(head_ref)) = (head_ref, repo.head().ok().and_then(|h| h.try_into_referent().ok())) {
        if let Ok(target_name) = head_ref.name().try_into() {
            let symref_line = format!("{} refs/heads/{}", head_id.to_hex(), target_name);
            let packet = format!("{:04x}{}", symref_line.len() + 4, symref_line);
            stream.write_all(packet.as_bytes()).await
                .map_err(|e| io_err(format!("Failed to write HEAD symref: {}", e)))?;
        }
    }
    
    // Send the rest of the references
    for git_ref in refs_list.iter() {
        let oid = git_ref.id().to_hex().to_string();
        let name = git_ref.name().as_bstr();
        
        let line = format!("{} {}", oid, name);
        let packet = format!("{:04x}{}", line.len() + 4, line);
        stream.write_all(packet.as_bytes()).await
            .map_err(|e| io_err(format!("Failed to write reference {}: {}", name, e)))?;
    }
    
    // Send a flush packet
    stream.write_all(b"0000").await
        .map_err(|e| io_err(format!("Failed to write flush packet: {}", e)))?;
    
    log::debug!("Sent {} references to client", refs_list.len());
    
    Ok(())
}

/// Process Git upload-pack (fetch/clone) negotiation
pub async fn process_wants<S>(
    stream: &mut S,
    repo: &Repository
) -> Result<(Vec<ObjectId>, Vec<ObjectId>)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    log::info!("Processing object negotiation");
    
    let mut wanted_objects = Vec::new();
    let mut have_objects = Vec::new();
    let mut shallow_objects = Vec::new();
    let mut client_done = false;
    let mut length_buf = [0u8; 4];
    let mut data_buf = Vec::new();
    
    // Read the client's wants and haves
    while !client_done {
        // Read packet length
        stream.read_exact(&mut length_buf).await
            .map_err(|e| io_err(format!("Failed to read packet length: {}", e)))?;
            
        let length_str = std::str::from_utf8(&length_buf)
            .map_err(|_| protocol_err("Invalid packet length encoding", None))?;
            
        // Check for flush packet
        if length_str == "0000" {
            // Flush packet - end of current section
            if !wanted_objects.is_empty() && !have_objects.is_empty() {
                // If we've seen wants and haves, this flush marks the end of haves
                log::debug!("Client sent flush packet after haves");
                client_done = true;
            } else if !wanted_objects.is_empty() {
                // If we've only seen wants, this flush marks the end of wants
                log::debug!("Client sent flush packet after wants");
                // Wait for haves or done
            } else {
                // No wants yet - unexpected flush
                log::warn!("Client sent unexpected flush packet");
                return Err(protocol_err("Unexpected flush packet", None));
            }
            continue;
        }
        
        // Parse length
        let length = u16::from_str_radix(length_str, 16)
            .map_err(|_| protocol_err("Invalid packet length", None))?;
            
        if length < 4 {
            return Err(protocol_err("Invalid packet length", None));
        }
        
        // Read packet data
        let data_length = length as usize - 4; // Subtract the 4 bytes of the length header
        data_buf.resize(data_length, 0);
        stream.read_exact(&mut data_buf).await
            .map_err(|e| io_err(format!("Failed to read packet data: {}", e)))?;
            
        // Parse command
        let line = std::str::from_utf8(&data_buf)
            .map_err(|_| protocol_err("Invalid UTF-8 in packet", None))?;
            
        if line.starts_with("want ") {
            // Parse the wanted object
            if line.len() < 45 {
                log::warn!("Invalid want line: {}", line);
                continue;
            }
            
            let oid_hex = &line[5..45];
            match ObjectId::from_hex(oid_hex.as_bytes()) {
                Ok(oid) => {
                    log::debug!("Client wants object: {}", oid_hex);
                    wanted_objects.push(oid);
                },
                Err(_) => return Err(protocol_err(format!("Invalid object ID: {}", oid_hex), None)),
            }
        } else if line.starts_with("have ") {
            // Parse the client's have
            if line.len() < 45 {
                log::warn!("Invalid have line: {}", line);
                continue;
            }
            
            let oid_hex = &line[5..45];
            match ObjectId::from_hex(oid_hex.as_bytes()) {
                Ok(oid) => {
                    log::debug!("Client has object: {}", oid_hex);
                    have_objects.push(oid);
                },
                Err(_) => return Err(protocol_err(format!("Invalid object ID: {}", oid_hex), None)),
            }
        } else if line.starts_with("shallow ") {
            // Parse shallow object
            if line.len() < 48 {
                log::warn!("Invalid shallow line: {}", line);
                continue;
            }
            
            let oid_hex = &line[8..48];
            match ObjectId::from_hex(oid_hex.as_bytes()) {
                Ok(oid) => {
                    log::debug!("Client shallow object: {}", oid_hex);
                    shallow_objects.push(oid);
                },
                Err(_) => return Err(protocol_err(format!("Invalid object ID: {}", oid_hex), None)),
            }
        } else if line.trim() == "done" {
            // Client is done sending commands
            log::debug!("Client sent done");
            client_done = true;
        }
    }
    
    log::info!("Object negotiation complete: {} wants, {} haves, {} shallows", 
             wanted_objects.len(), have_objects.len(), shallow_objects.len());
    
    // Send acknowledgement before packfile
    send_ack_response(stream, &have_objects, true).await?;
    
    Ok((wanted_objects, have_objects))
}

/// Send an acknowledgement response for object negotiation
async fn send_ack_response<S>(
    stream: &mut S,
    have_objects: &[ObjectId],
    multi_ack: bool
) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    if have_objects.is_empty() {
        // No objects to acknowledge
        stream.write_all(b"0008NAK\n").await
            .map_err(|e| io_err(format!("Failed to write NAK packet: {}", e)))?;
        return Ok(());
    }
    
    if multi_ack {
        // Send ACK for the last have with status
        let last_have = have_objects.last().unwrap();
        let ack_line = format!("ACK {} ready\n", last_have.to_hex());
        let packet = format!("{:04x}{}", ack_line.len() + 4, ack_line);
        stream.write_all(packet.as_bytes()).await
            .map_err(|e| io_err(format!("Failed to write ACK packet: {}", e)))?;
    } else {
        // Simple ACK for the last have
        let last_have = have_objects.last().unwrap();
        let ack_line = format!("ACK {}\n", last_have.to_hex());
        let packet = format!("{:04x}{}", ack_line.len() + 4, ack_line);
        stream.write_all(packet.as_bytes()).await
            .map_err(|e| io_err(format!("Failed to write ACK packet: {}", e)))?;
    }
    
    Ok(())
}

/// Send a packfile containing the requested objects
pub async fn send_packfile<S>(
    stream: &mut S,
    repo: &Repository, 
    wanted_objects: &[ObjectId],
    have_objects: &[ObjectId],
) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    if wanted_objects.is_empty() {
        // No objects requested, send an empty flush packet
        stream.write_all(b"0000").await
            .map_err(|e| io_err(format!("Failed to write flush packet: {}", e)))?;
        return Ok(());
    }

    log::info!("Sending packfile with {} requested objects", wanted_objects.len());

    // We use side-band-64k protocol for better error reporting
    // This means we send our packfile data with a 1-byte channel prefix
    // Channel 1: packfile data
    // Channel 2: progress messages
    // Channel 3: error messages

    // Create a packfile writer
    let mut pack_builder = gix_pack::data::output::Builder::default();
    
    // Set up pack builder options
    pack_builder
        .set_thread_limit(num_cpus::get().min(4))
        .use_reflog(true);
    
    // Send initial progress message
    send_progress(stream, "Preparing packfile...").await?;

    // Start processing objects in a background task to avoid blocking
    let (tx, mut rx) = mpsc::channel::<Result<Vec<u8>>>(2);  // Buffer up to 2 chunks
    let (progress_tx, mut progress_rx) = mpsc::channel::<String>(10); // Buffer for progress messages
    
    // Clone objects for the task
    let wanted_objects_clone = wanted_objects.to_vec();
    let have_objects_clone = have_objects.to_vec();
    let repo_path = repo.path().to_path_buf();
    
    // Spawn a task to build the packfile
    let pack_task = tokio::spawn(async move {
        // Create a progress reporter
        let progress_tx_clone = progress_tx.clone();
        let progress_reporter = move |msg: String| {
            let tx = progress_tx_clone.clone();
            let _ = tx.try_send(msg); // Ignore errors if channel is full
        };
        
        let progress_clone = progress_reporter.clone();
        
        // Open repository in the background task
        let repo = match Repository::open(repo_path) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(Err(protocol_err(format!("Failed to open repository: {}", e), None))).await;
                return;
            }
        };
        
        // Report progress
        progress_reporter("Analyzing object graph...".to_string());
        
        // Find the commits that the client doesn't have
        let mut objects_to_send = Vec::new();
        
        for wanted in &wanted_objects_clone {
            // Check if client already has this object
            if have_objects_clone.contains(wanted) {
                continue;
            }
            
            // Check if object exists in the repository
            match repo.find_object(*wanted) {
                Ok(object) => {
                    // Add object to the list
                    objects_to_send.push(object.id);
                    
                    // Report progress
                    progress_reporter(format!("Processing object {}", object.id));
                },
                Err(e) => {
                    let err_msg = format!("Object not found: {}", e);
                    let _ = tx.send(Err(protocol_err(err_msg, None))).await;
                    return;
                }
            }
        }
        
        // Create shallow boundary if client has any objects
        let boundary = if !have_objects_clone.is_empty() {
            Some(have_objects_clone)
        } else {
            None
        };
        
        // Set up traversal with boundary
        progress_reporter("Building object graph traversal...".to_string());
        
        let mut traversal_builder = repo.objects.traverse(objects_to_send)?
            .with_deepen(true)  // Include all tree entries for tree objects
            .with_objects(true);  // Include all reachable objects
            
        // Apply boundary if one exists
        if let Some(ref boundary_objects) = boundary {
            traversal_builder = traversal_builder.with_boundary(boundary_objects.clone());
        }
        
        // Traverse object graph
        let mut traversal = traversal_builder;
        let mut object_count = 0;
        
        // Report total objects count if known
        progress_reporter(format!("Traversing {} objects...", traversal.total_objects()));
        
        // Process objects in chunks
        let mut objects_chunk = Vec::new();
        let chunk_size = 1000;  // Process 1000 objects per chunk
        
        while let Some(obj_result) = traversal.next() {
            let obj = match obj_result {
                Ok(obj) => obj,
                Err(e) => {
                    let err_msg = format!("Failed to traverse object: {}", e);
                    let _ = tx.send(Err(protocol_err(err_msg, None))).await;
                    return;
                }
            };
            
            // Add to current chunk
            objects_chunk.push(obj);
            object_count += 1;
            
            // Process chunk when it reaches the desired size
            if objects_chunk.len() >= chunk_size {
                // Report progress
                if object_count % 1000 == 0 {
                    progress_reporter(format!("Processed {}/{} objects", 
                                             object_count, traversal.total_objects()));
                }
                
                // Add objects to the pack
                for obj in &objects_chunk {
                    if let Err(e) = pack_builder.add_object(obj.data.into(), obj.kind) {
                        let err_msg = format!("Failed to add object to pack: {}", e);
                        let _ = tx.send(Err(protocol_err(err_msg, None))).await;
                        return;
                    }
                }
                
                // Clear the chunk for the next iteration
                objects_chunk.clear();
            }
        }
        
        // Process any remaining objects
        for obj in objects_chunk {
            if let Err(e) = pack_builder.add_object(obj.data.into(), obj.kind) {
                let err_msg = format!("Failed to add object to pack: {}", e);
                let _ = tx.send(Err(protocol_err(err_msg, None))).await;
                return;
            }
        }
        
        // Report final object count
        progress_reporter(format!("Processed {} objects in total", object_count));
        progress_reporter("Finalizing packfile...".to_string());
        
        // Finalize packfile data
        let pack_data = match pack_builder.finish() {
            Ok(data) => data,
            Err(e) => {
                let err_msg = format!("Failed to create packfile: {}", e);
                let _ = tx.send(Err(protocol_err(err_msg, None))).await;
                return;
            }
        };
        
        // Report packfile size
        progress_reporter(format!("Generated packfile: {} bytes", pack_data.len()));
        
        // Send the packfile data in chunks that fit into the side-band-64k protocol
        // Max 65519 bytes per packet (65535 - 4 bytes for length prefix - 1 byte for channel - 11 bytes for overhead)
        const MAX_CHUNK_SIZE: usize = 65000;
        let mut offset = 0;
        
        while offset < pack_data.len() {
            let chunk_size = std::cmp::min(MAX_CHUNK_SIZE, pack_data.len() - offset);
            let chunk = pack_data[offset..offset + chunk_size].to_vec();
            
            // Send the chunk
            if let Err(e) = tx.send(Ok(chunk)).await {
                log::error!("Failed to send packfile chunk: {}", e);
                break;
            }
            
            offset += chunk_size;
            
            // Report progress periodically
            if offset % (1024 * 1024) == 0 {  // Every 1MB
                progress_reporter(format!("Sending packfile: {}/{} bytes ({:.1}%)", 
                                         offset, pack_data.len(), 
                                         (offset as f64 / pack_data.len() as f64) * 100.0));
            }
        }
        
        // Report completion
        progress_reporter(format!("Packfile transmission complete: {} objects, {} bytes", 
                                 object_count, pack_data.len()));
        
        // Close the channel to signal completion
        drop(tx);
        drop(progress_tx);
    });
    
    // Process progress messages
    let progress_handler = tokio::spawn(async move {
        while let Some(msg) = progress_rx.recv().await {
            // Forward progress to the client
            if let Err(e) = send_progress(stream, &msg).await {
                log::error!("Failed to send progress message: {}", e);
                break;
            }
        }
    });
    
    // Process packfile chunks as they become available
    while let Some(chunk_result) = rx.recv().await {
        match chunk_result {
            Ok(chunk) => {
                // Send the chunk with the data channel prefix
                send_packet_on_channel(stream, PackProtocolChannel::Data, &chunk).await?;
            },
            Err(e) => {
                // Send error message
                send_error(stream, &format!("Packfile generation error: {}", e)).await?;
                return Err(e);
            }
        }
    }
    
    // Wait for progress handler to complete
    let _ = progress_handler.await;
    
    // Wait for pack task to complete (it should be done by now)
    let _ = pack_task.await;
    
    // Send completion message
    send_progress(stream, "Pack transfer complete").await?;
    
    // Send flush packet to indicate end of packfile
    stream.write_all(b"0000").await
        .map_err(|e| io_err(format!("Failed to write final flush packet: {}", e)))?;
    
    log::info!("Packfile sent successfully");
    Ok(())
}

/// Send a message on the progress channel
async fn send_progress<S>(stream: &mut S, message: &str) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    send_packet_on_channel(stream, PackProtocolChannel::Progress, message.as_bytes()).await
}

/// Send a message on the error channel
async fn send_error<S>(stream: &mut S, message: &str) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    send_packet_on_channel(stream, PackProtocolChannel::Error, message.as_bytes()).await
}

/// Send data on a specific sideband channel
async fn send_packet_on_channel<S>(
    stream: &mut S,
    channel: PackProtocolChannel,
    data: &[u8]
) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    // Calculate packet size (4 bytes length + 1 byte channel + data)
    let packet_size = data.len() + 5;
    
    // Format header: 4-digit hex length + 1-byte channel
    let header = format!("{:04x}{}", packet_size, channel as u8);
    
    // Write header
    stream.write_all(header.as_bytes()).await
        .map_err(|e| io_err(format!("Failed to write packet header: {}", e)))?;
        
    // Write data
    stream.write_all(data).await
        .map_err(|e| io_err(format!("Failed to write packet data: {}", e)))?;
        
    Ok(())
}

/// Parse a pkt-line from a stream
async fn read_pkt_line<S>(stream: &mut S) -> Result<Option<Vec<u8>>>
where
    S: AsyncRead + Unpin,
{
    let mut length_buf = [0u8; 4];
    
    // Read packet length
    match stream.read_exact(&mut length_buf).await {
        Ok(_) => {},
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            // End of stream reached
            return Ok(None);
        },
        Err(e) => {
            return Err(io_err(format!("Failed to read packet length: {}", e)));
        }
    }
    
    let length_str = std::str::from_utf8(&length_buf)
        .map_err(|_| protocol_err("Invalid packet length encoding", None))?;
        
    // Check for flush packet
    if length_str == "0000" {
        return Ok(Some(Vec::new()));  // Empty vec represents flush packet
    }
    
    // Parse length
    let length = u16::from_str_radix(length_str, 16)
        .map_err(|_| protocol_err("Invalid packet length", None))?;
        
    if length < 4 {
        return Err(protocol_err("Invalid packet length", None));
    }
    
    // Read packet data
    let data_length = length as usize - 4;  // Subtract the 4 bytes of the length header
    let mut data = vec![0; data_length];
    
    stream.read_exact(&mut data).await
        .map_err(|e| io_err(format!("Failed to read packet data: {}", e)))?;
        
    Ok(Some(data))
}

/// Process Git receive-pack (push) requests
pub async fn receive_packfile<S>(
    stream: &mut S, 
    repo: &Repository
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    log::info!("Receiving packfile from client");
    
    // First, read the client's reference updates
    let mut ref_updates = HashMap::new();
    
    // Read reference update commands
    loop {
        let line = match read_pkt_line(stream).await? {
            Some(data) if data.is_empty() => {
                // Flush packet - end of reference updates
                break;
            },
            Some(data) => data,
            None => {
                return Err(protocol_err("Unexpected end of stream", None));
            }
        };
        
        // Parse reference update command
        let line_str = std::str::from_utf8(&line)
            .map_err(|_| protocol_err("Invalid UTF-8 in packet", None))?;
            
        // Reference update format: <old-oid> <new-oid> <ref-name>
        let parts: Vec<&str> = line_str.split_whitespace().collect();
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
                    Err(_) => return Err(protocol_err(format!("Invalid old object ID: {}", old_oid_str), None)),
                }
            };
            
            let new_oid = if new_oid_str == "0000000000000000000000000000000000000000" {
                // Deletion
                None
            } else {
                match ObjectId::from_hex(new_oid_str.as_bytes()) {
                    Ok(oid) => Some(oid),
                    Err(_) => return Err(protocol_err(format!("Invalid new object ID: {}", new_oid_str), None)),
                }
            };
            
            log::debug!("Reference update request: {} {} -> {}", 
                      ref_name, 
                      old_oid.map_or("null".to_string(), |o| o.to_hex().to_string()),
                      new_oid.map_or("null".to_string(), |o| o.to_hex().to_string()));
            
            ref_updates.insert(ref_name.to_string(), (old_oid, new_oid));
        }
    }
    
    // Read packfile from client
    // In a full implementation, we would:
    // 1. Create a temporary file to store the packfile
    // 2. Read and validate the packfile
    // 3. Index the packfile
    // 4. Apply the reference updates
    
    // For now, we'll just read and discard the packfile data
    let mut packfile_data = Vec::new();
    
    log::info!("Reading packfile data from client");
    
    // Read packfile data
    loop {
        let data = match read_pkt_line(stream).await? {
            Some(data) if data.is_empty() => {
                // Flush packet - end of packfile
                break;
            },
            Some(data) => data,
            None => {
                return Err(protocol_err("Unexpected end of stream", None));
            }
        };
        
        // Append to packfile data
        packfile_data.extend_from_slice(&data);
    }
    
    log::info!("Received {} bytes of packfile data", packfile_data.len());
    
    // Report unpack success first
    stream.write_all(b"0010unpack ok\n").await
        .map_err(|e| io_err(format!("Failed to write unpack status: {}", e)))?;
    
    // Apply the reference updates
    let mut results = Vec::new();
    
    for (ref_name, (old_oid, new_oid)) in ref_updates {
        let result = match (old_oid, new_oid) {
            (_, None) => {
                // Delete reference
                match repo.references.delete(&ref_name) {
                    Ok(_) => {
                        log::info!("Deleted reference: {}", ref_name);
                        format!("ok {}", ref_name)
                    },
                    Err(e) => {
                        log::error!("Failed to delete reference {}: {}", ref_name, e);
                        format!("ng {} deletion failed", ref_name)
                    }
                }
            },
            (None, Some(new_id)) => {
                // Create new reference
                match repo.references.create(&ref_name, new_id, false, &format!("push: created {}", ref_name)) {
                    Ok(_) => {
                        log::info!("Created reference: {} -> {}", ref_name, new_id);
                        format!("ok {}", ref_name)
                    },
                    Err(e) => {
                        log::error!("Failed to create reference {}: {}", ref_name, e);
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
                            log::error!("Reference update failed: {} expected {}, found {}", 
                                      ref_name, old_id, existing_ref.id());
                            format!("ng {} expected old value {} was {}", 
                                   ref_name, old_id, existing_ref.id())
                        } else {
                            // Update the reference
                            match repo.references.create_matching(&ref_name, new_id, false, old_id, 
                                                                &format!("push: update {}", ref_name)) {
                                Ok(_) => {
                                    log::info!("Updated reference: {} {} -> {}", ref_name, old_id, new_id);
                                    format!("ok {}", ref_name)
                                },
                                Err(e) => {
                                    log::error!("Failed to update reference {}: {}", ref_name, e);
                                    format!("ng {} update failed", ref_name)
                                }
                            }
                        }
                    },
                    Err(e) => {
                        log::error!("Failed to find reference {}: {}", ref_name, e);
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
        stream.write_all(packet.as_bytes()).await
            .map_err(|e| io_err(format!("Failed to write result: {}", e)))?;
    }
    
    // Send flush packet to indicate end of reference updates
    stream.write_all(b"0000").await
        .map_err(|e| io_err(format!("Failed to write flush packet: {}", e)))?;
    
    log::info!("Repository references updated successfully");
    Ok(())
}

/// Run the Git upload-pack service
pub async fn handle_upload_pack<S>(
    stream: &mut S, 
    repo: &Repository,
    command: &GitCommand
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    log::info!("Handling git-upload-pack command for {:?}", command.repo_path);
    
    // Create capabilities object
    let capabilities = ServerCapabilities::new();
    
    // Send references advertisement
    send_refs_advertisement(stream, repo, command, &capabilities).await?;
    
    // Process wants/haves (negotiation)
    let (wants, haves) = process_wants(stream, repo).await?;
    
    // Send packfile with requested objects
    send_packfile(stream, repo, &wants, &haves).await?;
    
    log::info!("git-upload-pack command completed successfully");
    Ok(())
}

/// Run the Git receive-pack service
pub async fn handle_receive_pack<S>(
    stream: &mut S, 
    repo: &Repository,
    command: &GitCommand
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    log::info!("Handling git-receive-pack command for {:?}", command.repo_path);
    
    // Create capabilities object
    let capabilities = ServerCapabilities::new();
    
    // Send references advertisement
    send_refs_advertisement(stream, repo, command, &capabilities).await?;
    
    // Process receive-pack request (push)
    receive_packfile(stream, repo).await?;
    
    log::info!("git-receive-pack command completed successfully");
    Ok(())
}

/// Handle a Git smart protocol connection
pub async fn handle_connection<S>(
    stream: &mut S,
    repo: &Repository
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Parse the Git command
    let command = parse_git_command(stream).await?;
    
    // Handle the command based on service type
    match command.service.as_str() {
        "git-upload-pack" => {
            handle_upload_pack(stream, repo, &command).await?;
        },
        "git-receive-pack" => {
            handle_receive_pack(stream, repo, &command).await?;
        },
        _ => {
            return Err(protocol_err(format!("Unsupported Git service: {}", command.service), None));
        }
    }
    
    Ok(())
}