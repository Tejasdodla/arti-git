use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(feature = "tor")]
use arti_client::{TorClient, TorClientConfig};
#[cfg(feature = "tor")]
use tor_rtcompat::{Runtime, PreferredRuntime};

use gix::{Repository, open};
#[cfg(feature = "tor")]
use gix_transport::client::{connect, capabilities};

use crate::core::{ArtiGitConfig, GitError, Result, io_err, repo_err, transport_err};
#[cfg(feature = "tor")]
use crate::transport::{TorTransport, ArtiGitTransportRegistry, create_transport_registry};
use crate::utils;
#[cfg(feature = "ipfs")]
use crate::ipfs::{IpfsClient, IpfsObjectStorage, IpfsObjectProvider};

// Log setup
use std::sync::Once;
static LOGGER_INIT: Once = Once::new();

/// Initialize logging system
fn init_logging() {
    LOGGER_INIT.call_once(|| {
        // We'll use a basic logger for now
        // In a future implementation, we could use a more sophisticated logging system
        std::env::set_var("RUST_LOG", "info");
        if let Err(e) = env_logger::try_init() {
            eprintln!("Failed to initialize logger: {}", e);
        }
    });
}

/// Workaround for the gix-url canonicalization issue
fn canonicalize_url_path(url_str: &str) -> Result<String> {
    // Only process file:// URLs
    if !url_str.starts_with("file://") {
        return Ok(url_str.to_string());
    }
    
    // Extract the path portion
    let path_part = url_str.strip_prefix("file://").unwrap_or(url_str);
    
    // Convert to absolute path if needed
    let path = Path::new(path_part);
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| io_err(format!("Failed to get current directory: {}", e), "."))?
            .join(path)
    };
    
    // Convert back to URL format
    let canonical_url = format!("file://{}", abs_path.to_string_lossy());
    Ok(canonical_url)
}

/// The main ArtiGit client that integrates Arti (Tor) with gitoxide
pub struct ArtiGitClient {
    config: ArtiGitConfig,
    
    #[cfg(feature = "tor")]
    runtime: PreferredRuntime,
    #[cfg(feature = "tor")]
    tor_client: Option<Arc<TorClient<PreferredRuntime>>>,
    #[cfg(feature = "tor")]
    tor_transport: Option<Arc<TorTransport>>,
    #[cfg(feature = "tor")]
    transport_registry: Option<ArtiGitTransportRegistry>,
    #[cfg(feature = "tor")]
    transport_handle: Option<capabilities::TransportFactoryHandle>,
    
    /// IPFS client for interacting with the IPFS network
    #[cfg(feature = "ipfs")]
    ipfs_client: Option<Arc<IpfsClient>>,
    
    /// IPFS object storage for Git objects
    #[cfg(feature = "ipfs")]
    ipfs_storage: Option<Arc<IpfsObjectStorage>>,
}

impl ArtiGitClient {
    /// Create a new ArtiGit client using the provided configuration
    pub async fn new(config: ArtiGitConfig) -> Result<Self> {
        // Initialize logging
        init_logging();
        
        // Log client creation with config summary
        log::info!("Creating new ArtiGit client: Tor={}, IPFS={}", 
            config.tor.use_tor, config.ipfs.enabled);
            
        #[cfg(feature = "tor")]
        let runtime = PreferredRuntime::create()
            .map_err(|e| GitError::Transport(format!("Failed to create runtime: {}", e), None))?;
            
        #[cfg(feature = "tor")]
        let tor_client = if config.tor.use_tor {
            // Configure and bootstrap Tor client
            log::info!("Bootstrapping Tor client...");
            let arti_config = config.to_arti_config()?;
            
            let client = TorClient::create_bootstrapped(runtime.clone(), arti_config)
                .await
                .map_err(|e| GitError::Transport(format!("Failed to bootstrap Tor: {}", e), None))?;
                
            log::info!("Tor client bootstrapped successfully");
            Some(Arc::new(client))
        } else {
            log::debug!("Tor is disabled in configuration, skipping initialization");
            None
        };
        
        // Create transport and registry if Tor is enabled
        #[cfg(feature = "tor")]
        let (tor_transport, transport_registry, transport_handle) = if config.tor.use_tor {
            if let Some(client) = &tor_client {
                // Create the Tor transport
                log::info!("Creating Tor transport...");
                let transport = TorTransport::new(tor_client.as_ref().cloned())
                    .await
                    .map_err(|e| GitError::Transport(format!("Failed to create Tor transport: {}", e), None))?;
                let transport_arc = Arc::new(transport);
                
                // Create the transport registry
                let registry = create_transport_registry(transport_arc.clone())
                    .await
                    .map_err(|e| GitError::Transport(format!("Failed to create transport registry: {}", e), None))?;
                    
                // Register the transport
                let handle = registry.register();
                log::info!("Tor transport registered successfully");
                
                (Some(transport_arc), Some(registry), Some(handle))
            } else {
                log::warn!("Cannot create Tor transport: Tor client unavailable");
                (None, None, None)
            }
        } else {
            log::debug!("Skipping Tor transport creation (disabled in config)");
            (None, None, None)
        };
        
        // Initialize IPFS if enabled
        #[cfg(feature = "ipfs")]
        let (ipfs_client, ipfs_storage) = if config.ipfs.enabled {
            log::info!("Initializing IPFS client...");
            match IpfsClient::new(config.ipfs.clone()).await {
                Ok(client) => {
                    log::info!("IPFS client initialized successfully");
                    let client_arc = Arc::new(client);
                    
                    // Create the object storage
                    log::info!("Creating IPFS object storage...");
                    match IpfsObjectStorage::new(client_arc.clone()).await {
                        Ok(storage) => {
                            log::info!("IPFS object storage created successfully");
                            (Some(client_arc), Some(Arc::new(storage)))
                        },
                        Err(e) => {
                            log::error!("Failed to initialize IPFS object storage: {}", e);
                            (Some(client_arc), None)
                        }
                    }
                },
                Err(e) => {
                    log::error!("Failed to initialize IPFS client: {}", e);
                    (None, None)
                }
            }
        } else {
            log::debug!("IPFS is disabled in configuration, skipping initialization");
            (None, None)
        };
        
        #[cfg(not(feature = "ipfs"))]
        let _ = &config.ipfs.enabled;  // Just to use the variable
        
        #[cfg(feature = "tor")]
        let client = Self {
            config,
            runtime,
            tor_client,
            tor_transport,
            transport_registry,
            transport_handle,
            #[cfg(feature = "ipfs")]
            ipfs_client,
            #[cfg(feature = "ipfs")]
            ipfs_storage,
        };
        
        #[cfg(not(feature = "tor"))]
        let client = Self {
            config,
            #[cfg(feature = "ipfs")]
            ipfs_client,
            #[cfg(feature = "ipfs")]
            ipfs_storage,
        };
        
        log::info!("ArtiGit client created successfully");
        Ok(client)
    }
    
    /// Create a client with the default configuration
    pub async fn with_default_config() -> Result<Self> {
        let config = ArtiGitConfig::default();
        Self::new(config).await
    }
    
    /// Load a client configuration from a file
    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path_ref = path.as_ref();
        log::info!("Loading configuration from: {}", path_ref.display());
        
        let config = ArtiGitConfig::from_file(path_ref)?;
        Self::new(config).await
    }
    
    /// Clone a repository using the appropriate transport based on the URL
    pub async fn clone(&self, url: &str, path: impl AsRef<Path>) -> Result<Repository> {
        let path_ref = path.as_ref();
        log::info!("Cloning repository from '{}' to '{}'", url, path_ref.display());
        
        // Process the URL to make file:// URLs absolute without using gix-url's problematic method
        let canonical_url = canonicalize_url_path(url)?;
        log::debug!("Canonical URL: {}", canonical_url);
            
        // Clone using gitoxide's standard API
        let repo = Repository::clone(canonical_url.clone(), path_ref)
            .map_err(|e| repo_err(format!("Clone failed: {}", e), path_ref))?;
            
        log::info!("Repository cloned successfully to: {}", path_ref.display());
        Ok(repo)
    }
    
    /// Open an existing repository
    pub fn open(&self, path: impl AsRef<Path>) -> Result<Repository> {
        let path_ref = path.as_ref();
        log::debug!("Opening repository at: {}", path_ref.display());
        
        open(path_ref)
            .map_err(|e| repo_err(format!("Failed to open repository: {}", e), path_ref))
    }
    
    /// Pull updates for a repository
    pub async fn pull(&self, repo: &mut Repository) -> Result<()> {
        // Get repository path for better error reporting
        let repo_path = repo.path().to_path_buf();
        log::info!("Pulling updates for repository: {}", repo_path.display());
        
        // Get the default remote
        let remote_name = "origin"; // We could make this configurable
        log::debug!("Using remote: {}", remote_name);
        
        // Create a fetch operation
        let mut remote = repo.remote(remote_name)
            .map_err(|e| repo_err(format!("Failed to get remote '{}': {}", remote_name, e), &repo_path))?;
        
        // Get remote URL for better error reporting
        let remote_url = remote.url()
            .map_err(|e| repo_err(format!("Failed to get remote URL: {}", e), &repo_path))?
            .to_string();
        log::debug!("Remote URL: {}", remote_url);
        
        // Fetch from remote - transport will be automatically selected based on URL
        log::info!("Fetching from remote: {}", remote_name);
        let result = gix::interrupt::init_handler(|| {});
        remote.fetch(&gix::fetch::Options::default(), &result)
            .map_err(|e| transport_err(format!("Failed to fetch from remote: {}", e), remote_url))?;
            
        log::info!("Fetch completed successfully");
        
        // For now, just perform the fetch. In a full implementation, we'd also handle merging.
        log::debug!("Note: Pull operation currently only fetches updates, merge not implemented yet");
        Ok(())
    }
    
    /// Push changes to a remote repository
    pub async fn push(&self, repo: &Repository, remote: Option<&str>, refspec: Option<&str>) -> Result<()> {
        // Get repository path for better error reporting
        let repo_path = repo.path().to_path_buf();
        
        // Get the specified remote, or default to 'origin'
        let remote_name = remote.unwrap_or("origin");
        log::info!("Pushing to remote '{}' from repository: {}", remote_name, repo_path.display());
        
        // Create a push operation
        let mut remote = repo.remote(remote_name)
            .map_err(|e| repo_err(format!("Failed to get remote '{}': {}", remote_name, e), &repo_path))?;
        
        // Get remote URL for better error reporting
        let remote_url = remote.url()
            .map_err(|e| repo_err(format!("Failed to get remote URL: {}", e), &repo_path))?
            .to_string();
        log::debug!("Remote URL: {}", remote_url);
        
        // Push to remote
        let mut options = gix::push::Options::default();
        
        // If a specific refspec was provided, use it
        if let Some(spec) = refspec {
            log::debug!("Using custom refspec: {}", spec);
            // Parse the refspec
            let push_spec = gix::remote::pushspec::parse(spec)
                .map_err(|e| GitError::InvalidArgument(format!("Invalid refspec '{}': {}", spec, e)))?;
            options.specs = vec![push_spec];
        }
        
        // Perform the push - transport will be automatically selected based on URL
        log::info!("Pushing to remote: {}", remote_name);
        let result = remote.push(&options)
            .map_err(|e| transport_err(format!("Push failed: {}", e), remote_url))?;
        
        // Check for errors
        if result.has_errors() {
            log::error!("Push had errors: {:?}", result);
            return Err(repo_err(format!("Push had errors: {:?}", result), repo_path));
        }
        
        log::info!("Push completed successfully");
        Ok(())
    }
    
    /// Add files to the Git index
    pub async fn add(&self, repo: &Repository, paths: &[PathBuf]) -> Result<()> {
        let repo_path = repo.path().to_path_buf();
        log::info!("Adding files to index in repository: {}", repo_path.display());
        
        let mut index = repo.index()
            .map_err(|e| repo_err(format!("Failed to get repository index: {}", e), &repo_path))?;
        
        // Track number of files added for logging
        let mut added_count = 0;
        
        for path in paths {
            log::debug!("Processing path: {}", path.display());
            
            // Handle path patterns and wildcards
            if path.to_string_lossy().contains('*') {
                log::debug!("Path contains wildcard pattern: {}", path.display());
                // Use pathspec to handle glob patterns
                let pattern = gix_pathspec::Pattern::new(path.to_string_lossy())
                    .map_err(|e| GitError::InvalidArgument(format!("Invalid path pattern: {}", e)))?;
                    
                let workdir = repo.work_dir()
                    .map_err(|e| repo_err(format!("Failed to get work directory: {}", e), &repo_path))?;
                    
                let matches = pattern.matches_in_directory(&workdir)
                    .map_err(|e| io_err(format!("Failed to match path pattern: {}", e), workdir))?;
                
                log::debug!("Pattern '{}' matched {} files", path.display(), matches.len());
                
                for matched_path in matches {
                    log::debug!("Adding matched file: {}", matched_path.display());
                    index.add_path(&matched_path)
                        .map_err(|e| io_err(format!("Failed to add path {}: {}", 
                                                    matched_path.display(), e), &matched_path))?;
                    added_count += 1;
                }
            } else {
                // Add single file
                log::debug!("Adding single file: {}", path.display());
                index.add_path(path)
                    .map_err(|e| io_err(format!("Failed to add path {}: {}", 
                                                path.display(), e), path))?;
                added_count += 1;
            }
        }
        
        // Write the updated index
        log::debug!("Writing updated index with {} added files", added_count);
        index.write()
            .map_err(|e| repo_err(format!("Failed to write index: {}", e), &repo_path))?;
        
        log::info!("Successfully added {} files to the index", added_count);
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
    
    #[cfg(feature = "tor")]
    /// Get the Tor client instance, if available
    pub fn tor_client(&self) -> Option<Arc<TorClient<PreferredRuntime>>> {
        self.tor_client.clone()
    }
    
    #[cfg(feature = "tor")]
    /// Get the runtime instance
    pub fn runtime(&self) -> PreferredRuntime {
        self.runtime.clone()
    }
    
    #[cfg(feature = "tor")]
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
    
    #[cfg(feature = "ipfs")]
    /// Get the IPFS client, if available
    pub fn ipfs_client(&self) -> Option<Arc<IpfsClient>> {
        self.ipfs_client.clone()
    }
    
    #[cfg(feature = "ipfs")]
    /// Get the IPFS object storage, if available
    pub fn ipfs_storage(&self) -> Option<Arc<IpfsObjectStorage>> {
        self.ipfs_storage.clone()
    }
    
    #[cfg(feature = "ipfs")]
    /// Check if IPFS is enabled and available
    pub fn is_ipfs_enabled(&self) -> bool {
        self.ipfs_client.is_some() && self.ipfs_storage.is_some()
    }
    
    #[cfg(feature = "ipfs")]
    /// Store a file in IPFS
    pub async fn store_in_ipfs(&self, path: impl AsRef<Path>) -> Result<String> {
        let client = self.ipfs_client.as_ref()
            .ok_or_else(|| GitError::Config("IPFS is not enabled".to_string()))?;
            
        client.add_file(path).await
    }
    
    #[cfg(feature = "ipfs")]
    /// Store raw data in IPFS
    pub async fn store_bytes_in_ipfs(&self, data: &[u8]) -> Result<String> {
        let client = self.ipfs_client.as_ref()
            .ok_or_else(|| GitError::Config("IPFS is not enabled".to_string()))?;
            
        client.add_bytes(data).await
    }
    
    #[cfg(feature = "ipfs")]
    /// Retrieve a file from IPFS by its content ID (CID)
    pub async fn get_from_ipfs(&self, cid: &str) -> Result<bytes::Bytes> {
        let client = self.ipfs_client.as_ref()
            .ok_or_else(|| GitError::Config("IPFS is not enabled".to_string()))?;
            
        client.get_file(cid).await
    }
    
    /// Get the LFS client, if available
    pub fn lfs_client(&self) -> Option<Arc<crate::lfs::LfsClient>> {
        // Check if LFS is enabled in the config
        if !self.config.lfs.enabled {
            return None;
        }
        
        // Create the LFS client on-demand if it's enabled
        if self.config.lfs.enabled {
            let config = self.config.lfs.clone();
            
            // Try to create a new LFS client
            match crate::lfs::LfsClient::new(config) {
                Ok(client) => {
                    #[cfg(feature = "ipfs")]
                    // If IPFS is configured, create the client with IPFS support
                    if self.config.ipfs.enabled && self.config.lfs.use_ipfs {
                        if let Some(ipfs_client) = &self.ipfs_client {
                            if let Ok(lfs_client) = crate::lfs::LfsClient::with_ipfs(
                                self.config.lfs.clone(),
                                ipfs_client.clone()
                            ) {
                                return Some(Arc::new(lfs_client));
                            }
                        }
                    }
                    
                    // Return the client without IPFS support
                    Some(Arc::new(client))
                },
                Err(e) => {
                    eprintln!("Warning: Failed to create LFS client: {}", e);
                    None
                }
            }
        } else {
            None
        }
    }
    
    /// Get the LFS storage backend, if available
    pub fn lfs_storage(&self) -> Option<Arc<crate::lfs::LfsStorage>> {
        // Check if LFS is enabled in the config
        if !self.config.lfs.enabled {
            return None;
        }
        
        // Create the LFS storage on-demand using the configured directory
        let base_dir = if self.config.lfs.objects_dir.is_absolute() {
            self.config.lfs.objects_dir.clone()
        } else {
            // Use a default directory if not configured
            let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("~/.local/share"));
            path.push("arti-git");
            path.push("lfs");
            path.push("objects");
            path
        };
        
        #[cfg(feature = "ipfs")]
        // Try to create a new LFS storage with IPFS support
        if self.config.ipfs.enabled && self.config.lfs.use_ipfs {
            // Create with IPFS support
            if let Some(ipfs_client) = &self.ipfs_client {
                match crate::lfs::LfsStorage::with_ipfs(
                    base_dir.clone(), 
                    ipfs_client.clone(), 
                    self.config.lfs.ipfs_primary
                ) {
                    Ok(storage) => return Some(Arc::new(storage)),
                    Err(e) => {
                        eprintln!("Warning: Failed to create LFS storage with IPFS: {}", e);
                        // Fall back to local-only storage
                    }
                }
            }
        }
        
        // Create local-only storage
        match crate::lfs::LfsStorage::new(base_dir) {
            Ok(storage) => Some(Arc::new(storage)),
            Err(e) => {
                eprintln!("Warning: Failed to create LFS storage: {}", e);
                None
            }
        }
    }
    
    /// Initialize Git LFS for a repository
    pub async fn init_lfs(&self, repo_path: impl AsRef<Path>) -> Result<()> {
        crate::lfs::configure_lfs(self, repo_path).await
    }
    
    /// Track a file pattern with Git LFS
    pub async fn lfs_track(&self, pattern: &str, repo_path: impl AsRef<Path>) -> Result<()> {
        crate::lfs::track(self, pattern, repo_path).await
    }
    
    /// Start an LFS server for serving LFS objects
    pub async fn start_lfs_server(&self, addr: &str, base_url: &str, repo_dir: impl AsRef<Path>) -> Result<()> {
        crate::lfs::start_server(self, addr, base_url, repo_dir).await
    }
}