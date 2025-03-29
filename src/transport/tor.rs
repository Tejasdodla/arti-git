use std::sync::Arc;
use std::fmt;
use std::io;
use bytes::{Bytes, BytesMut};
use url::{Url, ParseError};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncRead, AsyncWrite};

use arti_client::{TorClient, TorClientConfig, StreamPrefs};
use arti_client::DataStream;
use tor_rtcompat::PreferredRuntime;
use gix_url::Url as GixUrl;
use gix_transport::client::{Transport, RequestWriter, GetRequest, FetchRequest};
use gix_protocol::{fetch, transport};

use crate::core::{GitError, Result, ObjectId, ObjectType, RemoteConnection};
use crate::protocol::{parse_git_command, process_wants, receive_packfile};
use crate::utils;

/// A transport for Git operations over the Tor network
#[derive(Clone)]
pub struct TorTransport {
    tor_client: Arc<TorClient<PreferredRuntime>>,
    stream_prefs: StreamPrefs,
}

impl TorTransport {
    /// Create a new TorTransport instance
    pub async fn new(tor_client: Option<Arc<TorClient<PreferredRuntime>>>) -> Result<Self> {
        let client = match tor_client {
            Some(client) => client,
            None => {
                // Initialize a new Tor client if one wasn't provided
                let config = TorClientConfig::default();
                let client = TorClient::create_bootstrapped(config)
                    .await
                    .map_err(|e| GitError::Transport(format!("Failed to bootstrap Tor: {}", e)))?;
                Arc::new(client)
            }
        };
        
        let stream_prefs = StreamPrefs::default();
        
        Ok(Self {
            tor_client: client,
            stream_prefs,
        })
    }
    
    /// Check if the URL should be handled by this transport
    pub fn handles_url(url: &str) -> bool {
        if let Ok(parsed_url) = Url::parse(url) {
            let host = match parsed_url.host_str() {
                Some(h) => h,
                None => return false,
            };
            
            // Handle .onion addresses
            if host.ends_with(".onion") {
                return true;
            }
            
            // Handle tor+* URL schemes
            if parsed_url.scheme().starts_with("tor+") {
                return true;
            }
        }
        
        false
    }
    
    /// Create a Tor connection to the given host:port
    async fn connect(&self, host: &str, port: u16) -> Result<DataStream> {
        let address = format!("{}:{}", host, port);
        
        println!("Connecting to {} over Tor", address);
        
        self.tor_client.connect(&address, &self.stream_prefs)
            .await
            .map_err(|e| GitError::Transport(format!("Failed to connect to {}: {}", address, e)))
    }
    
    /// Extract host and port from a URL
    fn parse_url(&self, url: &str) -> Result<(String, u16)> {
        let parsed_url = Url::parse(url)
            .map_err(|e| GitError::Transport(format!("Invalid URL: {}", e)))?;
            
        // Handle tor+* URL schemes
        let host = match parsed_url.host_str() {
            Some(h) => h.to_string(),
            None => return Err(GitError::Transport("Missing host in URL".to_string())),
        };
        
        // Get port or use default port based on scheme
        let port = match parsed_url.port() {
            Some(p) => p,
            None => {
                match parsed_url.scheme() {
                    "git" | "tor+git" => 9418, // Git protocol default port
                    "http" | "tor+http" => 80, // HTTP default port
                    "https" | "tor+https" => 443, // HTTPS default port
                    _ => return Err(GitError::Transport(format!("Unsupported scheme: {}", parsed_url.scheme()))),
                }
            }
        };
        
        // For tor+* schemes, we need to extract the real hostname from the URL path
        let real_host = if parsed_url.scheme().starts_with("tor+") {
            let mut path_segments = parsed_url.path_segments().unwrap_or_else(|| "".split('/'));
            let first_segment = path_segments.next().unwrap_or("");
            
            // If the first path segment looks like a hostname, use it
            if first_segment.contains('.') || first_segment.ends_with(".onion") {
                first_segment.to_string()
            } else {
                // Otherwise, use the original host
                host
            }
        } else {
            host
        };
        
        Ok((real_host, port))
    }
    
    /// Execute a Git upload-pack request (for clone/fetch)
    async fn upload_pack(&self, url: &str, request: &FetchRequest) -> Result<Vec<u8>> {
        let (host, port) = self.parse_url(url)?;
        
        // Connect to the remote server through Tor
        let mut stream = self.connect(&host, port).await?;
        
        // Construct the Git request
        let command = format!("git-upload-pack /{}\0host={}\0", 
                              utils::get_repo_path_from_url(url)?, host);
        
        // Send the request
        stream.write_all(command.as_bytes()).await
            .map_err(|e| GitError::Transport(format!("Failed to send git-upload-pack request: {}", e)))?;
            
        // Read server's response
        let mut buffer = Vec::new();
        stream.read_to_end(&mut buffer).await
            .map_err(|e| GitError::Transport(format!("Failed to read git-upload-pack response: {}", e)))?;
            
        Ok(buffer)
    }
    
    /// Execute a Git receive-pack request (for push)
    async fn receive_pack(&self, url: &str, request: &[u8]) -> Result<Vec<u8>> {
        let (host, port) = self.parse_url(url)?;
        
        // Connect to the remote server through Tor
        let mut stream = self.connect(&host, port).await?;
        
        // Construct the Git request
        let command = format!("git-receive-pack /{}\0host={}\0", 
                              utils::get_repo_path_from_url(url)?, host);
        
        // Send the request
        stream.write_all(command.as_bytes()).await
            .map_err(|e| GitError::Transport(format!("Failed to send git-receive-pack request: {}", e)))?;
            
        // Send the push request data
        stream.write_all(request).await
            .map_err(|e| GitError::Transport(format!("Failed to send git-receive-pack data: {}", e)))?;
            
        // Read server's response
        let mut buffer = Vec::new();
        stream.read_to_end(&mut buffer).await
            .map_err(|e| GitError::Transport(format!("Failed to read git-receive-pack response: {}", e)))?;
            
        Ok(buffer)
    }
}

impl fmt::Debug for TorTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TorTransport")
            .field("tor_client", &"Arc<TorClient>")
            .field("stream_prefs", &"StreamPrefs")
            .finish()
    }
}

/// A connection to a Git repository over Tor
pub struct TorConnection {
    url: String,
    onion_address: String,
    port: u16,
    client: Option<Arc<TorClient<PreferredRuntime>>>,
    capabilities: Vec<String>,
}

impl TorConnection {
    /// Create a new Tor connection (non-blocking)
    pub fn new(url: &str) -> Result<Self> {
        let parsed_url = Url::parse(url)
            .map_err(|e| GitError::Transport(format!("Invalid URL: {}", e)))?;
            
        // Extract onion address and port
        let host = parsed_url.host_str()
            .ok_or_else(|| GitError::Transport("Missing host in URL".to_string()))?;
            
        if !host.ends_with(".onion") {
            return Err(GitError::Transport(format!("Not an onion address: {}", host)));
        }
        
        let port = parsed_url.port().unwrap_or(80);
        
        Ok(Self {
            url: url.to_string(),
            onion_address: host.to_string(),
            port,
            client: None,
            capabilities: Vec::new(),
        })
    }
    
    /// Initialize the Tor client (must be called before other operations)
    pub async fn init(&mut self) -> Result<()> {
        // Configure and bootstrap Tor client
        let config = TorClientConfig::default();
        
        let tor_client = TorClient::create_bootstrapped(config)
            .await
            .map_err(|e| GitError::Transport(format!("Failed to bootstrap Tor: {}", e)))?;
            
        self.client = Some(Arc::new(tor_client));
        Ok(())
    }
    
    /// Create a new Tor stream to the specified onion service
    async fn create_stream(&self) -> Result<DataStream> {
        let client = self.client.as_ref()
            .ok_or_else(|| GitError::Transport("Tor client not initialized".to_string()))?;
        
        let prefs = StreamPrefs::default();
        let addr = format!("{}:{}", self.onion_address, self.port);
        
        client.connect(&addr, &prefs)
            .await
            .map_err(|e| GitError::Transport(format!("Failed to connect to {}: {}", addr, e)))
    }
    
    /// Discover references from the remote repository
    async fn discover_refs(&mut self) -> Result<Vec<(String, ObjectId)>> {
        // Establish connection
        let mut stream = self.create_stream().await?;
        
        // Send git-upload-pack request
        let repo_path = utils::get_repo_path_from_url(&self.url)?;
        let command = format!("git-upload-pack /{}\0host={}\0", 
                             repo_path, self.onion_address);
        
        stream.write_all(command.as_bytes()).await
            .map_err(|e| GitError::Transport(format!("Failed to send git-upload-pack request: {}", e)))?;
            
        // Read and parse the response
        // In a full implementation, we would parse the reference advertisement
        let mut refs = Vec::new();
        
        // For now, return an empty list
        // In a full implementation, we would parse the reference advertisement here
        
        // Close connection
        stream.close().await
            .map_err(|e| GitError::Transport(format!("Failed to close Tor stream: {}", e)))?;
            
        Ok(refs)
    }
}

// Implementation for GitOxide Transport integration
impl Transport for TorTransport {
    fn request(
        &self, 
        url: &gix_url::Url, 
        service: transport::Service, 
        args: Vec<transport::client::Argument<'_>>,
        initial_response_of_fetch: Option<fetch::Response>
    ) -> std::result::Result<Box<dyn RequestWriter>, gix_transport::client::Error> {
        // Clone the transport for the async task
        let this = self.clone();
        let url_string = url.to_string();
        
        // Create a runtime to execute async operations synchronously
        let runtime = tokio::runtime::Runtime::new().map_err(|e| {
            gix_transport::client::Error::Io(io::Error::new(
                io::ErrorKind::Other, 
                format!("Failed to create Tokio runtime: {}", e)
            ))
        })?;
        
        // Handle different Git services
        match service {
            transport::Service::UploadPack => {
                // Create the fetch request
                let fetch_request = FetchRequest {
                    url: url_string.clone(),
                    // Add other fields as needed
                };
                
                // Execute the upload-pack request
                let response = runtime.block_on(async {
                    this.upload_pack(&url_string, &fetch_request).await.map_err(|e| {
                        gix_transport::client::Error::Io(io::Error::new(
                            io::ErrorKind::Other, 
                            format!("Tor upload-pack error: {}", e)
                        ))
                    })
                })?;
                
                // Create a request writer with the response
                let writer = TorRequestWriter::new(response);
                Ok(Box::new(writer))
            },
            transport::Service::ReceivePack => {
                // Create an empty request writer for now, it will be filled when write() is called
                let writer = TorRequestWriter::new(Vec::new());
                Ok(Box::new(writer))
            },
            _ => {
                Err(gix_transport::client::Error::Io(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("Unsupported service: {:?}", service)
                )))
            }
        }
    }
    
    fn supports_url(url: &GixUrl) -> bool {
        Self::handles_url(&url.to_string())
    }
}

/// A request writer for Tor transport
pub struct TorRequestWriter {
    data: Vec<u8>,
    written: bool,
}

impl TorRequestWriter {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            written: false,
        }
    }
}

impl RequestWriter for TorRequestWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        // In a real implementation, we would send this data to the server
        // For now, we just store it
        self.written = true;
        self.data.extend_from_slice(data);
        Ok(data.len())
    }
    
    fn response(&mut self) -> std::io::Result<&[u8]> {
        if !self.written {
            // If no data has been written, return initial response
            Ok(&self.data)
        } else {
            // In a real implementation, this would receive a response from the server
            // after the write operation
            // For now, we just return the data we have
            Ok(&self.data)
        }
    }
}

// Implement Write trait for TorRequestWriter
impl io::Write for TorRequestWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        RequestWriter::write(self, buf)
    }
    
    fn flush(&mut self) -> io::Result<()> {
        // No-op as we don't buffer
        Ok(())
    }
}

impl AsyncRemoteConnection for TorConnection {
    async fn list_refs_async(&mut self) -> Result<Vec<(String, ObjectId)>> {
        self.discover_refs().await
    }
    
    async fn fetch_objects_async(&mut self, wants: &[ObjectId], haves: &[ObjectId]) 
        -> Result<Vec<(ObjectType, ObjectId, Bytes)>> {
        
        // Create a new Tor stream
        let mut stream = self.create_stream().await?;
        
        // Send git-upload-pack request
        let repo_path = utils::get_repo_path_from_url(&self.url)?;
        let command = format!("git-upload-pack /{}\0host={}\0", 
                             repo_path, self.onion_address);
        
        stream.write_all(command.as_bytes()).await
            .map_err(|e| GitError::Transport(format!("Failed to send git-upload-pack request: {}", e)))?;
        
        // In a full implementation, we would:
        // 1. Parse the reference advertisement
        // 2. Send our "want" and "have" lines
        // 3. Receive and parse the packfile
        
        // For now, return empty list
        
        // Close connection
        stream.close().await
            .map_err(|e| GitError::Transport(format!("Failed to close Tor stream: {}", e)))?;
            
        Ok(Vec::new())
    }
    
    async fn push_objects_async(&mut self, objects: &[(ObjectType, ObjectId, Bytes)], refs: &[(String, ObjectId)]) 
        -> Result<()> {
        
        // Create a new Tor stream
        let mut stream = self.create_stream().await?;
        
        // Send git-receive-pack request
        let repo_path = utils::get_repo_path_from_url(&self.url)?;
        let command = format!("git-receive-pack /{}\0host={}\0", 
                             repo_path, self.onion_address);
        
        stream.write_all(command.as_bytes()).await
            .map_err(|e| GitError::Transport(format!("Failed to send git-receive-pack request: {}", e)))?;
        
        // In a full implementation, we would:
        // 1. Parse the reference advertisement
        // 2. Send our reference updates
        // 3. Send the packfile with objects
        // 4. Parse the server response
        
        // Close connection
        stream.close().await
            .map_err(|e| GitError::Transport(format!("Failed to close Tor stream: {}", e)))?;
            
        Ok(())
    }
}

/// An async implementation of RemoteConnection for Tor
/// Note: This is separate from the synchronous RemoteConnection trait
pub trait AsyncRemoteConnection {
    async fn list_refs_async(&mut self) -> Result<Vec<(String, ObjectId)>>;
    async fn fetch_objects_async(&mut self, wants: &[ObjectId], haves: &[ObjectId]) 
        -> Result<Vec<(ObjectType, ObjectId, Bytes)>>;
    async fn push_objects_async(&mut self, objects: &[(ObjectType, ObjectId, Bytes)], refs: &[(String, ObjectId)]) 
        -> Result<()>;
}

// Synchronous adapter for the standard RemoteConnection trait
// This allows us to use TorConnection with the existing RemoteConnection interface
impl RemoteConnection for TorConnection {
    fn list_refs(&mut self) -> Result<Vec<(String, ObjectId)>> {
        // Run async operation in a new Tokio runtime
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| GitError::Transport(format!("Failed to create runtime: {}", e)))?;
            
        rt.block_on(self.list_refs_async())
    }
    
    fn fetch_objects(&mut self, wants: &[ObjectId], haves: &[ObjectId]) 
        -> Result<Vec<(ObjectType, ObjectId, Bytes)>> {
        
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| GitError::Transport(format!("Failed to create runtime: {}", e)))?;
            
        rt.block_on(self.fetch_objects_async(wants, haves))
    }
    
    fn push_objects(&mut self, objects: &[(ObjectType, ObjectId, Bytes)], refs: &[(String, ObjectId)]) -> Result<()> {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| GitError::Transport(format!("Failed to create runtime: {}", e)))?;
            
        rt.block_on(self.push_objects_async(objects, refs))
    }
}