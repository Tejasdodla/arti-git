use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::net::SocketAddr;
use std::io;

use arti_client::{TorClient, OnionServiceConfig};
use tor_rtcompat::{Runtime, PreferredRuntime};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use gix::Repository;

use crate::core::{GitError, Result, OnionServiceConfig as ArtiGitOnionConfig};
use crate::protocol::{GitCommand, parse_git_command, send_refs_advertisement, 
                     process_wants, send_packfile, receive_packfile, update_references};
use crate::utils;

/// Git repository onion service
pub struct GitOnionService<R: Runtime> {
    /// The directory containing Git repositories to serve
    repo_dir: PathBuf,
    
    /// Tor client
    tor_client: Arc<TorClient<R>>,
    
    /// Service configuration
    config: ArtiGitOnionConfig,
    
    /// Runtime for async operations
    runtime: R,
    
    /// The onion address (once created)
    onion_address: Option<String>,
}

impl<R: Runtime> GitOnionService<R> {
    /// Create a new Git onion service
    pub fn new(
        tor_client: Arc<TorClient<R>>,
        repo_dir: impl AsRef<Path>,
        config: ArtiGitOnionConfig,
        runtime: R,
    ) -> Result<Self> {
        let repo_dir = utils::absolute_path(repo_dir)?;
        
        // Ensure the repository directory exists
        utils::ensure_dir_exists(&repo_dir)?;
        
        // Ensure the key directory exists
        utils::ensure_dir_exists(&config.key_dir)?;
        
        Ok(Self {
            repo_dir,
            tor_client,
            config,
            runtime,
            onion_address: None,
        })
    }
    
    /// Start the onion service
    pub async fn start(&mut self) -> Result<String> {
        // Bind to localhost on the configured port for local service
        let addr = SocketAddr::from(([127, 0, 0, 1], self.config.port));
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| GitError::IO(format!("Failed to bind to {}: {}", addr, e)))?;
            
        println!("Local Git service listening on {}", addr);
        
        // Configure the onion service
        let onion_config = OnionServiceConfig::builder()
            .nickname("arti-git")
            .key_path(self.config.key_dir.join("hs_ed25519_secret_key"))
            .build()
            .map_err(|e| GitError::Config(format!("Failed to build onion service config: {}", e)))?;
            
        // Create and publish the onion service
        let publish_handle = self.tor_client.publish_onion_service(
            onion_config, 
            [(self.config.port, addr)].into_iter()
        )
        .await
        .map_err(|e| GitError::Transport(format!("Failed to publish onion service: {}", e)))?;
        
        // Get the onion address
        let onion_addr = publish_handle.onion_name().to_string();
        println!("Onion service published at: {}", onion_addr);
        self.onion_address = Some(onion_addr.clone());
        
        // Start the local server that handles Git protocols
        let repo_dir = self.repo_dir.clone();
        
        // Spawn a task to handle incoming connections
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        println!("New connection from {}", addr);
                        let repo_path = repo_dir.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_git_connection(stream, &repo_path).await {
                                eprintln!("Error handling connection: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("Error accepting connection: {}", e);
                        break;
                    }
                }
            }
        });
        
        Ok(onion_addr)
    }
    
    /// Get the onion address of this service
    pub fn onion_address(&self) -> Option<&str> {
        self.onion_address.as_deref()
    }
}

/// Handle a Git client connection using our full Git protocol implementation
async fn handle_git_connection<S, P>(mut stream: S, repo_dir: &P) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    P: AsRef<Path>,
{
    // Parse the Git command from the client
    let command = match parse_git_command(&mut stream).await {
        Ok(cmd) => {
            println!("Received Git command: {} for path: {}", 
                   cmd.service, cmd.repo_path.display());
            cmd
        },
        Err(e) => {
            eprintln!("Error parsing Git command: {}", e);
            return Err(e);
        }
    };
    
    // Determine the full repository path
    let full_repo_path = repo_dir.as_ref().join(&command.repo_path);
    
    // Verify that the requested repository exists and is within our repos directory
    if !full_repo_path.exists() {
        let error_msg = format!("Repository not found: {}", command.repo_path.display());
        eprintln!("{}", error_msg);
        return Err(io::Error::new(io::ErrorKind::NotFound, error_msg));
    }
    
    // Ensure the repository path is within our served directory (security check)
    match utils::is_path_within(&full_repo_path, repo_dir) {
        Ok(is_within) => {
            if !is_within {
                let error_msg = format!("Security violation: Attempted access outside repo dir: {}", 
                                      full_repo_path.display());
                eprintln!("{}", error_msg);
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, error_msg));
            }
        },
        Err(e) => {
            let error_msg = format!("Path check error: {}", e);
            eprintln!("{}", error_msg);
            return Err(io::Error::new(io::ErrorKind::Other, error_msg));
        }
    }
    
    // Try to open the repository with gitoxide
    let repo = match gix::open(&full_repo_path) {
        Ok(repo) => repo,
        Err(e) => {
            let error_msg = format!("Failed to open repository {}: {}", full_repo_path.display(), e);
            eprintln!("{}", error_msg);
            return Err(io::Error::new(io::ErrorKind::NotFound, error_msg));
        }
    };
    
    // Handle the Git service based on the command
    match command.service.as_str() {
        "git-upload-pack" => {
            println!("Processing git-upload-pack request (clone/fetch operation)");
            
            // Send capabilities and references
            if let Err(e) = send_refs_advertisement(&mut stream, &repo, "git-upload-pack", true).await {
                eprintln!("Failed to send refs advertisement: {}", e);
                return Err(e);
            }
            
            // Process the client's wants and haves
            let wanted_objects = match process_wants(&mut stream, &repo).await {
                Ok(objects) => objects,
                Err(e) => {
                    eprintln!("Failed to process wants: {}", e);
                    return Err(e);
                }
            };
            
            println!("Client wants {} objects", wanted_objects.len());
            
            if !wanted_objects.is_empty() {
                // Send the requested objects as a packfile
                if let Err(e) = send_packfile(&mut stream, &repo, &wanted_objects).await {
                    eprintln!("Failed to send packfile: {}", e);
                    return Err(e);
                }
            }
            
            println!("Upload-pack operation completed successfully");
        },
        "git-receive-pack" => {
            println!("Processing git-receive-pack request (push operation)");
            
            // Send initial reference advertisement
            if let Err(e) = send_refs_advertisement(&mut stream, &repo, "git-receive-pack", true).await {
                eprintln!("Failed to send refs advertisement: {}", e);
                return Err(e);
            }
            
            // Receive packfile with new objects
            if let Err(e) = receive_packfile(&mut stream, &repo).await {
                eprintln!("Failed to receive packfile: {}", e);
                return Err(e);
            }
            
            println!("Receive-pack operation completed successfully");
        },
        _ => {
            // Unknown Git service
            let error_msg = format!("Unsupported Git service: {}", command.service);
            eprintln!("{}", error_msg);
            return Err(io::Error::new(io::ErrorKind::InvalidInput, error_msg));
        }
    }
    
    println!("Git operation completed successfully for {}", command.repo_path.display());
    Ok(())
}