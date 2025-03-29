use std::sync::Arc;
use std::fmt;
use bytes::Bytes;
use url::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use arti_client::{TorClient, TorClientConfig, StreamPrefs};
use arti_client::DataStream;
use tor_rtcompat::PreferredRuntime;

use crate::core::{GitError, Result, ObjectId, ObjectType, RemoteConnection};

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
        
        // TODO: Implement Git protocol over Tor
        // For simplicity, we'll return an empty list for now
        // In a full implementation, we would send git-upload-pack requests here
        
        // Close connection
        stream.close().await
            .map_err(|e| GitError::Transport(format!("Failed to close Tor stream: {}", e)))?;
            
        Ok(Vec::new())
    }
}

impl fmt::Debug for TorConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TorConnection")
            .field("url", &self.url)
            .field("onion_address", &self.onion_address)
            .field("port", &self.port)
            .field("client", &if self.client.is_some() { "Some(TorClient)" } else { "None" })
            .field("capabilities", &self.capabilities)
            .finish()
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

impl AsyncRemoteConnection for TorConnection {
    async fn list_refs_async(&mut self) -> Result<Vec<(String, ObjectId)>> {
        self.discover_refs().await
    }
    
    async fn fetch_objects_async(&mut self, wants: &[ObjectId], haves: &[ObjectId]) 
        -> Result<Vec<(ObjectType, ObjectId, Bytes)>> {
        
        // Create a new Tor stream
        let mut stream = self.create_stream().await?;
        
        // TODO: Implement Git pack protocol over Tor
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
        
        // TODO: Implement Git push protocol over Tor
        // For now, just return success
        
        // Close connection
        stream.close().await
            .map_err(|e| GitError::Transport(format!("Failed to close Tor stream: {}", e)))?;
            
        Ok(())
    }
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