use std::path::{Path, PathBuf};
use std::sync::Arc;

use arti_client::{TorClient, TorClientConfig};
use gix::{Repository, open};
use gix_url::Url;
use gix_transport::client::{connect, capabilities};
use tor_rtcompat::{Runtime, PreferredRuntime};

use crate::core::{ArtiGitConfig, GitError, Result};
use crate::transport::{TorTransport, ArtiGitTransportRegistry, create_transport_registry};
use crate::utils;
use crate::ipfs::{IpfsClient, IpfsObjectStorage, IpfsObjectProvider};

/// The main ArtiGit client that integrates Arti (Tor) with gitoxide
pub struct ArtiGitClient {
    config: ArtiGitConfig,
    runtime: PreferredRuntime,
    tor_client: Option<Arc<TorClient<PreferredRuntime>>>,
    tor_transport: Option<Arc<TorTransport>>,
    transport_registry: Option<ArtiGitTransportRegistry>,
    transport_handle: Option<capabilities::TransportFactoryHandle>,
    
    /// IPFS client for interacting with the IPFS network
    ipfs_client: Option<Arc<IpfsClient>>,
    
    /// IPFS object storage for Git objects
    ipfs_storage: Option<Arc<IpfsObjectStorage>>,
}

impl ArtiGitClient {
    /// Create a new ArtiGit client using the provided configuration
    pub async fn new(config: ArtiGitConfig) -> Result<Self> {
        let runtime = PreferredRuntime::create()
            .map_err(|e| GitError::Transport(format!("Failed to create runtime: {}", e)))?;
            
        let tor_client = if config.tor.use_tor {
            // Configure and bootstrap Tor client
            let arti_config = config.to_arti_config()?;
            
            let client = TorClient::create_bootstrapped(runtime.clone(), arti_config)
                .await
                .map_err(|e| GitError::Transport(format!("Failed to bootstrap Tor: {}", e)))?;
                
            Some(Arc::new(client))
        } else {
            None
        };
        
        // Create transport and registry if Tor is enabled
        let (tor_transport, transport_registry, transport_handle) = if config.tor.use_tor {
            if let Some(client) = &tor_client {
                // Create the Tor transport
                let transport = TorTransport::new(tor_client.as_ref().cloned())
                    .await
                    .map_err(|e| GitError::Transport(format!("Failed to create Tor transport: {}", e)))?;
                let transport_arc = Arc::new(transport);
                
                // Create the transport registry
                let registry = create_transport_registry(transport_arc.clone())
                    .await
                    .map_err(|e| GitError::Transport(format!("Failed to create transport registry: {}", e)))?;
                    
                // Register the transport
                let handle = registry.register();
                
                (Some(transport_arc), Some(registry), Some(handle))
            } else {
                (None, None, None)
            }
        } else {
            (None, None, None)
        };
        
        // Initialize IPFS if enabled
        let (ipfs_client, ipfs_storage) = if config.ipfs.enabled {
            match IpfsClient::new(config.ipfs.clone()).await {
                Ok(client) => {
                    let client_arc = Arc::new(client);
                    
                    // Create the object storage
                    match IpfsObjectStorage::new(client_arc.clone()).await {
                        Ok(storage) => (Some(client_arc), Some(Arc::new(storage))),
                        Err(e) => {
                            eprintln!("Warning: Failed to initialize IPFS object storage: {}", e);
                            (Some(client_arc), None)
                        }
                    }
                },
                Err(e) => {
                    eprintln!("Warning: Failed to initialize IPFS client: {}", e);
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
        
        Ok(Self {
            config,
            runtime,
            tor_client,
            tor_transport,
            transport_registry,
            transport_handle,
            ipfs_client,
            ipfs_storage,
        })
    }
    
    /// Create a client with the default configuration
    pub async fn with_default_config() -> Result<Self> {
        let config = ArtiGitConfig::default();
        Self::new(config).await
    }
    
    /// Load a client configuration from a file
    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let config = ArtiGitConfig::from_file(path.as_ref())?;
        Self::new(config).await
    }
    
    /// Clone a repository using the appropriate transport based on the URL
    pub async fn clone(&self, url: &str, path: impl AsRef<Path>) -> Result<Repository> {
        let url = Url::try_from(url)
            .map_err(|e| GitError::InvalidArgument(format!("Invalid URL: {}", e)))?;
        
        // With our registered transport, we don't need special handling anymore
        // The transport registry automatically selects the right transport based on URL
        let path = path.as_ref();
        
        // Clone using gitoxide's standard API
        let repo = Repository::clone(url.to_string(), path)
            .map_err(|e| GitError::Repository(format!("Clone failed: {}", e)))?;
            
        Ok(repo)
    }
    
    /// Open an existing repository
    pub fn open(&self, path: impl AsRef<Path>) -> Result<Repository> {
        open(path)
            .map_err(|e| GitError::Repository(format!("Failed to open repository: {}", e)))
    }
    
    /// Pull updates for a repository
    pub async fn pull(&self, repo: &mut Repository) -> Result<()> {
        // Get the default remote
        let remote_name = "origin"; // We could make this configurable
        
        // Create a fetch operation
        let mut remote = repo.remote(remote_name)
            .map_err(|e| GitError::Repository(format!("Failed to get remote '{}': {}", remote_name, e)))?;
        
        // Fetch from remote - transport will be automatically selected based on URL
        let result = gix::interrupt::init_handler(|| {});
        remote.fetch(&gix::fetch::Options::default(), &result)
            .map_err(|e| GitError::Repository(format!("Failed to fetch from remote: {}", e)))?;
            
        // For now, just perform the fetch. In a full implementation, we'd also handle merging.
        Ok(())
    }
    
    /// Push changes to a remote repository
    pub async fn push(&self, repo: &Repository, remote: Option<&str>, refspec: Option<&str>) -> Result<()> {
        // Get the specified remote, or default to 'origin'
        let remote_name = remote.unwrap_or("origin");
        
        // Create a push operation
        let mut remote = repo.remote(remote_name)
            .map_err(|e| GitError::Repository(format!("Failed to get remote '{}': {}", remote_name, e)))?;
        
        // Push to remote
        let mut options = gix::push::Options::default();
        
        // If a specific refspec was provided, use it
        if let Some(spec) = refspec {
            // Parse the refspec
            let push_spec = gix::remote::pushspec::parse(spec)
                .map_err(|e| GitError::InvalidArgument(format!("Invalid refspec '{}': {}", spec, e)))?;
            options.specs = vec![push_spec];
        }
        
        // Perform the push - transport will be automatically selected based on URL
        let result = remote.push(&options)
            .map_err(|e| GitError::Repository(format!("Push failed: {}", e)))?;
        
        // Check for errors
        if result.has_errors() {
            return Err(GitError::Repository(format!("Push had errors: {:?}", result)));
        }
        
        Ok(())
    }
    
    /// Add files to the Git index
    pub async fn add(&self, repo: &Repository, paths: &[PathBuf]) -> Result<()> {
        let mut index = repo.index()
            .map_err(|e| GitError::Repository(format!("Failed to get repository index: {}", e)))?;
        
        for path in paths {
            // Handle path patterns and wildcards
            if path.to_string_lossy().contains('*') {
                // Use pathspec to handle glob patterns
                let pattern = gix_pathspec::Pattern::new(path.to_string_lossy())
                    .map_err(|e| GitError::InvalidArgument(format!("Invalid path pattern: {}", e)))?;
                    
                let workdir = repo.work_dir()
                    .map_err(|e| GitError::Repository(format!("Failed to get work directory: {}", e)))?;
                    
                let matches = pattern.matches_in_directory(&workdir)
                    .map_err(|e| GitError::IO(format!("Failed to match path pattern: {}", e)))?;
                
                for matched_path in matches {
                    index.add_path(&matched_path)
                        .map_err(|e| GitError::Repository(format!("Failed to add path {}: {}", 
                                                                matched_path.display(), e)))?;
                }
            } else {
                // Add single file
                index.add_path(path)
                    .map_err(|e| GitError::Repository(format!("Failed to add path {}: {}", 
                                                            path.display(), e)))?;
            }
        }
        
        // Write the updated index
        index.write()
            .map_err(|e| GitError::Repository(format!("Failed to write index: {}", e)))?;
        
        Ok(())
    }
    
    /// Commit changes to the repository
    pub async fn commit(&self, repo: &Repository, message: &str, sign: bool) -> Result<gix_hash::ObjectId> {
        let committer = self.get_committer_from_config()?;
        let author = committer.clone();
        
        // Create commit builder
        let mut commit_builder = repo.commit_builder()
            .map_err(|e| GitError::Repository(format!("Failed to create commit builder: {}", e)))?;
        
        // Set basic commit properties
        commit_builder.author(author);
        commit_builder.committer(committer);
        commit_builder.message(message);
        
        // Sign the commit if requested
        if sign {
            let key = self.get_or_create_signing_key()?;
            commit_builder.sign(&key)
                .map_err(|e| GitError::Crypto(format!("Failed to sign commit: {}", e)))?;
        }
        
        // Create the commit
        let commit_id = commit_builder.commit()
            .map_err(|e| GitError::Repository(format!("Failed to create commit: {}", e)))?;
        
        Ok(commit_id)
    }
    
    /// Get committer information from configuration
    fn get_committer_from_config(&self) -> Result<gix_actor::SignatureRef<'static>> {
        // Get name and email from config, or use defaults
        let name = self.config.git.user_name.clone()
            .unwrap_or_else(|| "ArtiGit User".to_string());
            
        let email = self.config.git.user_email.clone()
            .unwrap_or_else(|| "user@artigit.invalid".to_string());
        
        // Create the signature
        Ok(gix_actor::SignatureRef {
            name: name.into(),
            email: email.into(),
            time: gix_date::Time::now_utc(),
        }.to_owned())
    }
    
    /// Get or create an Ed25519 key for signing
    fn get_or_create_signing_key(&self) -> Result<ed25519_dalek::Keypair> {
        use rand::Rng;
        
        // TODO: In a real implementation, we should store and load this key
        // For now, we'll just create a temporary one
        
        // Create a random seed
        let mut csprng = rand::thread_rng();
        let mut seed = [0u8; 32];
        csprng.fill(&mut seed);
        
        // Create keypair from seed
        let keypair = ed25519_dalek::Keypair::generate(&mut csprng);
        
        Ok(keypair)
    }
    
    /// Get the configuration
    pub fn config(&self) -> &ArtiGitConfig {
        &self.config
    }
    
    /// Get a mutable reference to the configuration
    pub fn config_mut(&mut self) -> &mut ArtiGitConfig {
        &mut self.config
    }
    
    /// Save the current configuration to a file
    pub fn save_config(&self, path: impl AsRef<Path>) -> Result<()> {
        self.config.save_to_file(path.as_ref())?;
        Ok(())
    }
    
    /// Get the Tor client instance, if available
    pub fn tor_client(&self) -> Option<Arc<TorClient<PreferredRuntime>>> {
        self.tor_client.clone()
    }
    
    /// Get the runtime instance
    pub fn runtime(&self) -> PreferredRuntime {
        self.runtime.clone()
    }
    
    /// Initialize and register the Tor transport
    async fn init_transport(&mut self) -> Result<()> {
        if self.config.tor.use_tor {
            println!("Initializing Tor transport...");
            
            // Create the Tor transport if it doesn't exist
            if self.tor_transport.is_none() {
                // Use the existing tor client if available
                let transport = TorTransport::new(self.tor_client.clone())
                    .await
                    .map_err(|e| GitError::Transport(format!("Failed to create Tor transport: {}", e)))?;
                    
                self.tor_transport = Some(Arc::new(transport));
            }
            
            // Initialize and register the transport
            if let Some(transport) = &self.tor_transport {
                // Using our new init_transport function from registry
                let handle = init_transport(transport.clone())
                    .await
                    .map_err(|e| GitError::Transport(format!("Failed to initialize transport: {}", e)))?;
                    
                self.transport_handle = Some(handle);
                
                println!("Tor transport initialized and registered");
            }
        } else {
            println!("Skipping Tor transport initialization (disabled in config)");
        }
        
        Ok(())
    }
    
    /// Get the IPFS client, if available
    pub fn ipfs_client(&self) -> Option<Arc<IpfsClient>> {
        self.ipfs_client.clone()
    }
    
    /// Get the IPFS object storage, if available
    pub fn ipfs_storage(&self) -> Option<Arc<IpfsObjectStorage>> {
        self.ipfs_storage.clone()
    }
    
    /// Check if IPFS is enabled and available
    pub fn is_ipfs_enabled(&self) -> bool {
        self.ipfs_client.is_some() && self.ipfs_storage.is_some()
    }
    
    /// Store a file in IPFS
    pub async fn store_in_ipfs(&self, path: impl AsRef<Path>) -> Result<String> {
        let client = self.ipfs_client.as_ref()
            .ok_or_else(|| GitError::Config("IPFS is not enabled".to_string()))?;
            
        client.add_file(path).await
    }
    
    /// Store raw data in IPFS
    pub async fn store_bytes_in_ipfs(&self, data: &[u8]) -> Result<String> {
        let client = self.ipfs_client.as_ref()
            .ok_or_else(|| GitError::Config("IPFS is not enabled".to_string()))?;
            
        client.add_bytes(data).await
    }
    
    /// Retrieve a file from IPFS by its content ID (CID)
    pub async fn get_from_ipfs(&self, cid: &str) -> Result<Bytes> {
        let client = self.ipfs_client.as_ref()
            .ok_or_else(|| GitError::Config("IPFS is not enabled".to_string()))?;
            
        client.get_file(cid).await
    }
    
    /// Store a Git object in IPFS
    pub async fn store_object_in_ipfs(&self, object_type: ObjectType, data: &[u8]) -> Result<ObjectId> {
        let storage = self.ipfs_storage.as_ref()
            .ok_or_else(|| GitError::Config("IPFS object storage is not enabled".to_string()))?;
            
        storage.store_object(object_type, data).await
    }
    
    /// Get a Git object from IPFS
    pub async fn get_object_from_ipfs(&self, id: &ObjectId) -> Result<(ObjectType, Bytes)> {
        let storage = self.ipfs_storage.as_ref()
            .ok_or_else(|| GitError::Config("IPFS object storage is not enabled".to_string()))?;
            
        storage.get_object(id).await
    }
}