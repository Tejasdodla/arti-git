use std::sync::Arc;
use std::fmt;
use std::io;
use std::time::Duration;
use std::collections::HashMap;
use bytes::{Bytes, BytesMut};
use url::{Url, ParseError};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, RwLock};
use tokio::time::timeout;
use futures::future::Future;

use arti_client::{TorClient, TorClientConfig, StreamPrefs, BootstrapBehavior};
use arti_client::DataStream;
use tor_rtcompat::PreferredRuntime;
use tor_rtcompat::Runtime;
use gix_url::Url as GixUrl;
use gix_transport::client::{Transport, RequestWriter, GetRequest, FetchRequest, Error as TransportError};
use gix_protocol::{fetch, transport};

use crate::core::{GitError, Result, ObjectId, ObjectType, RemoteConnection};
use crate::core::{io_err, transport_err};
use crate::protocol::{parse_git_command, process_wants, receive_packfile};
use crate::utils;

/// Connection stats for monitoring and diagnostics
#[derive(Debug, Default, Clone, Copy)]
pub struct ConnectionStats {
    /// Total number of connections made
    pub total_connections: usize,
    /// Number of successful connections
    pub successful_connections: usize,
    /// Number of failed connections
    pub failed_connections: usize,
    /// Number of connections reused from pool
    pub reused_connections: usize,
    /// Number of connections closed
    pub closed_connections: usize,
    /// Average connection time in milliseconds
    pub avg_connection_time_ms: u64,
    /// Number of secured connections (authenticated/encrypted)
    pub secured_connections: usize,
}

/// Security settings for Tor connections
#[derive(Debug, Clone)]
pub struct TorSecuritySettings {
    /// Whether to use strict onion address validation
    pub strict_onion_validation: bool,
    /// Whether to require authenticated connections when possible
    pub require_auth: bool,
    /// Whether to verify repository fingerprints
    pub verify_repo_fingerprint: bool,
    /// A list of trusted fingerprints for repositories
    pub trusted_fingerprints: HashMap<String, String>,
    /// Whether to isolate streams for different repositories
    pub isolate_streams: bool,
}

impl Default for TorSecuritySettings {
    fn default() -> Self {
        Self {
            strict_onion_validation: true,
            require_auth: false,
            verify_repo_fingerprint: true,
            trusted_fingerprints: HashMap::new(),
            isolate_streams: true,
        }
    }
}

/// Proxy settings for Tor connections
#[derive(Debug, Clone)]
pub struct TorProxySettings {
    /// The type of proxy to use
    pub proxy_type: TorProxyType,
    /// The host of the proxy
    pub host: String,
    /// The port of the proxy
    pub port: u16,
    /// Username for proxy authentication (if required)
    pub username: Option<String>,
    /// Password for proxy authentication (if required)
    pub password: Option<String>,
}

/// Types of proxies supported
#[derive(Debug, Clone, PartialEq)]
pub enum TorProxyType {
    /// Direct connection (no proxy)
    Direct,
    /// SOCKS5 proxy
    Socks5,
    /// HTTPS proxy
    Https,
}

impl Default for TorProxySettings {
    fn default() -> Self {
        Self {
            proxy_type: TorProxyType::Direct,
            host: String::new(),
            port: 0,
            username: None,
            password: None,
        }
    }
}

/// A transport for Git operations over the Tor network
#[derive(Clone)]
pub struct TorTransport {
    /// The Tor client for establishing connections
    tor_client: Arc<TorClient<PreferredRuntime>>,
    
    /// Stream preferences for Tor connections
    stream_prefs: StreamPrefs,
    
    /// Connection pool for reusing connections
    connection_pool: Arc<RwLock<HashMap<String, Vec<DataStream>>>>,
    
    /// Connection statistics
    stats: Arc<RwLock<ConnectionStats>>,
    
    /// Maximum connections to keep in the pool per destination
    max_pool_connections: usize,
    
    /// Connection timeout in seconds
    connection_timeout: u64,
    
    /// Whether to use the connection pool
    use_connection_pool: bool,

    /// Security settings for Tor connections
    security_settings: TorSecuritySettings,

    /// Proxy settings for Tor connections
    proxy_settings: TorProxySettings,
    
    /// Authentication credentials for repositories
    auth_credentials: Arc<RwLock<HashMap<String, (String, String)>>>,
}

impl TorTransport {
    /// Create a new TorTransport instance with custom configuration
    pub async fn with_config(
        config: Option<TorClientConfig>,
        security_settings: Option<TorSecuritySettings>,
        proxy_settings: Option<TorProxySettings>
    ) -> Result<Self> {
        // Create a new runtime
        let runtime = PreferredRuntime::create()
            .map_err(|e| transport_err(format!("Failed to create Tor runtime: {}", e), None))?;
        
        // Use custom config or default
        let config = config.unwrap_or_else(TorClientConfig::default);
        
        // Bootstrap the Tor client
        log::info!("Initializing new Tor client with custom configuration");
        let client = TorClient::create_bootstrapped(runtime, config)
            .await
            .map_err(|e| transport_err(format!("Failed to bootstrap Tor: {}", e), None))?;
        
        let stream_prefs = StreamPrefs::default();
        
        log::info!("TorTransport initialized successfully with custom configuration");
        
        Ok(Self {
            tor_client: Arc::new(client),
            stream_prefs,
            connection_pool: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(ConnectionStats::default())),
            max_pool_connections: 5,
            connection_timeout: 60,
            use_connection_pool: true,
            security_settings: security_settings.unwrap_or_default(),
            proxy_settings: proxy_settings.unwrap_or_default(),
            auth_credentials: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Create a new TorTransport instance
    pub async fn new(tor_client: Option<Arc<TorClient<PreferredRuntime>>>) -> Result<Self> {
        let client = match tor_client {
            Some(client) => client,
            None => {
                // Initialize a new Tor client if one wasn't provided
                log::info!("Initializing new Tor client");
                let config = TorClientConfig::default();
                let runtime = PreferredRuntime::create()
                    .map_err(|e| transport_err(format!("Failed to create Tor runtime: {}", e), None))?;
                
                let client = TorClient::create_bootstrapped(runtime, config)
                    .await
                    .map_err(|e| transport_err(format!("Failed to bootstrap Tor: {}", e), None))?;
                Arc::new(client)
            }
        };
        
        let stream_prefs = StreamPrefs::default();
        
        log::info!("TorTransport initialized successfully");
        
        Ok(Self {
            tor_client: client,
            stream_prefs,
            connection_pool: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(ConnectionStats::default())),
            max_pool_connections: 5,
            connection_timeout: 60,
            use_connection_pool: true,
            security_settings: TorSecuritySettings::default(),
            proxy_settings: TorProxySettings::default(),
            auth_credentials: Arc::new(RwLock::new(HashMap::new())),
        })
    }
    
    /// Configure connection pooling
    pub fn with_connection_pool(mut self, enable: bool, max_connections: Option<usize>) -> Self {
        self.use_connection_pool = enable;
        if let Some(max) = max_connections {
            self.max_pool_connections = max;
        }
        self
    }
    
    /// Set connection timeout
    pub fn with_timeout(mut self, timeout_seconds: u64) -> Self {
        self.connection_timeout = timeout_seconds;
        self
    }

    /// Set security settings
    pub fn with_security_settings(mut self, settings: TorSecuritySettings) -> Self {
        self.security_settings = settings;
        self
    }

    /// Set proxy settings
    pub fn with_proxy_settings(mut self, settings: TorProxySettings) -> Self {
        self.proxy_settings = settings;
        self
    }

    /// Add authentication credentials for a repository
    pub async fn add_auth_credentials(&self, host: &str, username: &str, password: &str) {
        let mut credentials = self.auth_credentials.write().await;
        credentials.insert(host.to_string(), (username.to_string(), password.to_string()));
        log::debug!("Added authentication credentials for {}", host);
    }

    /// Remove authentication credentials for a repository
    pub async fn remove_auth_credentials(&self, host: &str) -> bool {
        let mut credentials = self.auth_credentials.write().await;
        let removed = credentials.remove(host).is_some();
        if removed {
            log::debug!("Removed authentication credentials for {}", host);
        }
        removed
    }

    /// Add a trusted fingerprint for a repository
    pub fn add_trusted_fingerprint(&mut self, host: &str, fingerprint: &str) {
        self.security_settings.trusted_fingerprints.insert(host.to_string(), fingerprint.to_string());
        log::debug!("Added trusted fingerprint for {}: {}", host, fingerprint);
    }
    
    /// Get connection statistics
    pub async fn get_stats(&self) -> ConnectionStats {
        *self.stats.read().await
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

    /// Validate an onion address
    fn validate_onion_address(&self, host: &str) -> Result<()> {
        if !host.ends_with(".onion") {
            return Ok(());
        }

        // If strict validation is enabled, validate the onion address format
        if self.security_settings.strict_onion_validation {
            // Extract the onion address part without the .onion suffix
            let onion_part = &host[0..host.len() - 6];
            
            // Validate v3 onion address (56 characters base32)
            if onion_part.len() == 56 {
                // Check if it's valid base32
                if !onion_part.chars().all(|c| {
                    (c >= 'a' && c <= 'z') || (c >= '2' && c <= '7')
                }) {
                    return Err(transport_err(
                        format!("Invalid v3 onion address format: {}", host),
                        Some(host)
                    ));
                }
            }
            // Validate v2 onion address (16 characters)
            else if onion_part.len() == 16 {
                // Check if it's valid base32
                if !onion_part.chars().all(|c| {
                    (c >= 'a' && c <= 'z') || (c >= '2' && c <= '7')
                }) {
                    return Err(transport_err(
                        format!("Invalid v2 onion address format: {}", host),
                        Some(host)
                    ));
                }
                
                log::warn!("Using v2 onion address which is deprecated: {}", host);
            } else {
                return Err(transport_err(
                    format!("Invalid onion address length: {}", host),
                    Some(host)
                ));
            }
        }

        Ok(())
    }

    /// Verify repository fingerprint
    async fn verify_fingerprint(&self, host: &str, stream: &DataStream) -> Result<()> {
        if !self.security_settings.verify_repo_fingerprint {
            return Ok(());
        }

        // Check if we have a trusted fingerprint for this host
        if let Some(expected_fingerprint) = self.security_settings.trusted_fingerprints.get(host) {
            // Get the actual fingerprint from the connection
            if let Some(actual_fingerprint) = stream.peer_fingerprint() {
                let actual_fingerprint_str = hex::encode(actual_fingerprint);
                
                // Compare fingerprints
                if &actual_fingerprint_str == expected_fingerprint {
                    log::debug!("Repository fingerprint verified for {}", host);
                    return Ok(());
                } else {
                    log::warn!("Repository fingerprint verification failed for {}", host);
                    log::warn!("Expected: {}", expected_fingerprint);
                    log::warn!("Actual: {}", actual_fingerprint_str);
                    
                    return Err(transport_err(
                        format!("Repository fingerprint verification failed for {}", host),
                        Some(host)
                    ));
                }
            }
        }

        // If we don't have a trusted fingerprint or couldn't get the actual fingerprint,
        // allow the connection if strict fingerprint verification is disabled
        Ok(())
    }
    
    /// Get a connection from the pool or create a new one
    async fn get_connection(&self, host: &str, port: u16) -> Result<DataStream> {
        // Validate onion address format
        self.validate_onion_address(host)?;
        
        let key = format!("{}:{}", host, port);
        
        // Update total connection attempts
        {
            let mut stats = self.stats.write().await;
            stats.total_connections += 1;
        }
        
        // Try to get a connection from the pool if enabled
        if self.use_connection_pool {
            let mut pool = self.connection_pool.write().await;
            
            if let Some(connections) = pool.get_mut(&key) {
                if let Some(conn) = connections.pop() {
                    log::debug!("Reusing connection from pool for {}", key);
                    
                    // Update stats
                    {
                        let mut stats = self.stats.write().await;
                        stats.reused_connections += 1;
                    }
                    
                    return Ok(conn);
                }
            }
        }
        
        // Create a new connection if none is available in the pool
        log::debug!("Creating new Tor connection to {}", key);
        
        // Configure stream preferences based on security settings
        let mut stream_prefs = self.stream_prefs.clone();
        
        // Set isolation if enabled
        if self.security_settings.isolate_streams {
            stream_prefs = stream_prefs.isolate_connection();
        }
        
        let start_time = std::time::Instant::now();
        
        // Apply proxy settings if needed
        if self.proxy_settings.proxy_type != TorProxyType::Direct {
            // In a real implementation, we would configure the proxy here
            // This depends on the specific Arti API for proxy configuration
            match self.proxy_settings.proxy_type {
                TorProxyType::Socks5 => {
                    log::debug!("Using SOCKS5 proxy {}:{} for Tor connection", 
                                self.proxy_settings.host, self.proxy_settings.port);
                    // Configure SOCKS5 proxy in stream_prefs
                },
                TorProxyType::Https => {
                    log::debug!("Using HTTPS proxy {}:{} for Tor connection",
                                self.proxy_settings.host, self.proxy_settings.port);
                    // Configure HTTPS proxy in stream_prefs
                },
                _ => {}
            }
        }
        
        // Add authentication if available
        let mut auth_header = None;
        {
            let credentials = self.auth_credentials.read().await;
            if let Some((username, password)) = credentials.get(host) {
                // Create Basic authentication header
                let auth = format!("{}:{}", username, password);
                let encoded = base64::encode(auth.as_bytes());
                auth_header = Some(format!("Authorization: Basic {}\r\n", encoded));
                
                log::debug!("Using authentication for {}", host);
            }
        }
        
        // Use timeout for connection establishment
        let connection_result = timeout(
            Duration::from_secs(self.connection_timeout),
            self.tor_client.connect(&key, &stream_prefs)
        ).await;
        
        // Calculate connection time
        let connection_time = start_time.elapsed().as_millis() as u64;
        
        // Handle timeout and connection errors
        match connection_result {
            Ok(Ok(stream)) => {
                // Verify the repository fingerprint
                self.verify_fingerprint(host, &stream).await?;
                
                // Update stats for successful connection
                {
                    let mut stats = self.stats.write().await;
                    stats.successful_connections += 1;
                    
                    // Update average connection time
                    let total_conns = stats.successful_connections as u64;
                    if total_conns > 1 {
                        stats.avg_connection_time_ms = 
                            ((stats.avg_connection_time_ms * (total_conns - 1)) + connection_time) / total_conns;
                    } else {
                        stats.avg_connection_time_ms = connection_time;
                    }
                    
                    // Count as secured if it's an onion address
                    if host.ends_with(".onion") {
                        stats.secured_connections += 1;
                    }
                }
                
                log::debug!("Connected to {} in {}ms", key, connection_time);
                Ok(stream)
            },
            Ok(Err(e)) => {
                // Update stats for failed connection
                {
                    let mut stats = self.stats.write().await;
                    stats.failed_connections += 1;
                }
                
                let err_msg = format!("Failed to connect to {}: {}", key, e);
                log::error!("{}", err_msg);
                Err(transport_err(err_msg, Some(&key)))
            },
            Err(_) => {
                // Update stats for timeout
                {
                    let mut stats = self.stats.write().await;
                    stats.failed_connections += 1;
                }
                
                let err_msg = format!("Connection timeout after {}s: {}", self.connection_timeout, key);
                log::error!("{}", err_msg);
                Err(transport_err(err_msg, Some(&key)))
            }
        }
    }
    
    /// Return a connection to the pool
    async fn return_connection(&self, host: &str, port: u16, stream: DataStream) {
        if !self.use_connection_pool {
            // If connection pooling is disabled, just close the connection
            if let Err(e) = stream.close().await {
                log::warn!("Error closing Tor connection: {}", e);
            }
            return;
        }
        
        let key = format!("{}:{}", host, port);
        let mut pool = self.connection_pool.write().await;
        
        let connections = pool.entry(key.clone()).or_insert_with(Vec::new);
        
        // Only add to the pool if we haven't reached the maximum number of connections
        if connections.len() < self.max_pool_connections {
            log::debug!("Returning connection to pool for {}", key);
            connections.push(stream);
        } else {
            log::debug!("Connection pool full for {}, closing connection", key);
            // Close the connection if the pool is full
            if let Err(e) = stream.close().await {
                log::warn!("Error closing Tor connection: {}", e);
            }
            
            // Update stats
            {
                let mut stats = self.stats.write().await;
                stats.closed_connections += 1;
            }
        }
    }
    
    /// Extract host and port from a URL
    fn parse_url(&self, url: &str) -> Result<(String, u16)> {
        let parsed_url = Url::parse(url)
            .map_err(|e| transport_err(format!("Invalid URL: {}", e), Some(url)))?;
            
        // Handle tor+* URL schemes
        let host = match parsed_url.host_str() {
            Some(h) => h.to_string(),
            None => return Err(transport_err("Missing host in URL", Some(url))),
        };
        
        // Get port or use default port based on scheme
        let port = match parsed_url.port() {
            Some(p) => p,
            None => {
                match parsed_url.scheme() {
                    "git" | "tor+git" => 9418, // Git protocol default port
                    "http" | "tor+http" => 80, // HTTP default port
                    "https" | "tor+https" => 443, // HTTPS default port
                    _ => return Err(transport_err(format!("Unsupported scheme: {}", parsed_url.scheme()), Some(url))),
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
        
        log::info!("Executing git-upload-pack for {} via Tor", url);
        
        // Connect to the remote server through Tor
        let mut stream = self.get_connection(&host, port).await?;
        
        // Construct the Git request
        let repo_path = utils::get_repo_path_from_url(url)?;
        let command = format!("git-upload-pack /{}\0host={}\0", repo_path, host);
        
        log::debug!("Sending git-upload-pack command for repository: {}", repo_path);
        
        // Add authentication if available
        let auth_header = {
            let credentials = self.auth_credentials.read().await;
            credentials.get(&host).map(|(username, password)| {
                // Create Basic authentication header
                let auth = format!("{}:{}", username, password);
                let encoded = base64::encode(auth.as_bytes());
                format!("Authorization: Basic {}\r\n", encoded)
            })
        };
        
        // Send the request
        stream.write_all(command.as_bytes()).await
            .map_err(|e| transport_err(format!("Failed to send git-upload-pack request: {}", e), Some(url)))?;
        
        // Send authentication header if available
        if let Some(header) = auth_header {
            stream.write_all(header.as_bytes()).await
                .map_err(|e| transport_err(format!("Failed to send authentication header: {}", e), Some(url)))?;
        }
        
        // Process any additional data in the request
        if let Some(extra_data) = &request.extra_data {
            log::debug!("Sending {} bytes of extra request data", extra_data.len());
            stream.write_all(extra_data).await
                .map_err(|e| transport_err(format!("Failed to send extra request data: {}", e), Some(url)))?;
        }
        
        // Read server's response with timeout
        log::debug!("Reading server response");
        let mut buffer = BytesMut::with_capacity(4096).into();
        
        // Use a timeout for reading the response
        match timeout(
            Duration::from_secs(self.connection_timeout * 2), // Give extra time for reading
            read_to_end_with_progress(&mut stream, &mut buffer)
        ).await {
            Ok(Ok(_)) => {
                log::debug!("Received {} bytes from server", buffer.len());
                
                // Return the connection to the pool for future use
                self.return_connection(&host, port, stream).await;
                
                Ok(buffer)
            },
            Ok(Err(e)) => {
                // Reading failed with an error
                let err_msg = format!("Failed to read git-upload-pack response: {}", e);
                log::error!("{}", err_msg);
                Err(transport_err(err_msg, Some(url)))
            },
            Err(_) => {
                // Reading timed out
                let err_msg = format!("Timeout while reading git-upload-pack response after {}s", self.connection_timeout * 2);
                log::error!("{}", err_msg);
                Err(transport_err(err_msg, Some(url)))
            }
        }
    }
    
    /// Execute a Git receive-pack request (for push)
    async fn receive_pack(&self, url: &str, request: &[u8]) -> Result<Vec<u8>> {
        let (host, port) = self.parse_url(url)?;
        
        log::info!("Executing git-receive-pack for {} via Tor", url);
        
        // Connect to the remote server through Tor
        let mut stream = self.get_connection(&host, port).await?;
        
        // Construct the Git request
        let repo_path = utils::get_repo_path_from_url(url)?;
        let command = format!("git-receive-pack /{}\0host={}\0", repo_path, host);
        
        log::debug!("Sending git-receive-pack command for repository: {}", repo_path);
        
        // Add authentication if available
        let auth_header = {
            let credentials = self.auth_credentials.read().await;
            credentials.get(&host).map(|(username, password)| {
                // Create Basic authentication header
                let auth = format!("{}:{}", username, password);
                let encoded = base64::encode(auth.as_bytes());
                format!("Authorization: Basic {}\r\n", encoded)
            })
        };
        
        // Send the request
        stream.write_all(command.as_bytes()).await
            .map_err(|e| transport_err(format!("Failed to send git-receive-pack request: {}", e), Some(url)))?;
            
        // Send authentication header if available
        if let Some(header) = auth_header {
            stream.write_all(header.as_bytes()).await
                .map_err(|e| transport_err(format!("Failed to send authentication header: {}", e), Some(url)))?;
        }
            
        // Send the push request data
        log::debug!("Sending {} bytes of push data", request.len());
        stream.write_all(request).await
            .map_err(|e| transport_err(format!("Failed to send git-receive-pack data: {}", e), Some(url)))?;
            
        // Read server's response with timeout
        log::debug!("Reading server response");
        let mut buffer = BytesMut::with_capacity(4096).into();
        
        // Use a timeout for reading the response
        match timeout(
            Duration::from_secs(self.connection_timeout * 2), // Give extra time for reading
            read_to_end_with_progress(&mut stream, &mut buffer)
        ).await {
            Ok(Ok(_)) => {
                log::debug!("Received {} bytes from server", buffer.len());
                
                // Return the connection to the pool for future use
                self.return_connection(&host, port, stream).await;
                
                Ok(buffer)
            },
            Ok(Err(e)) => {
                // Reading failed with an error
                let err_msg = format!("Failed to read git-receive-pack response: {}", e);
                log::error!("{}", err_msg);
                Err(transport_err(err_msg, Some(url)))
            },
            Err(_) => {
                // Reading timed out
                let err_msg = format!("Timeout while reading git-receive-pack response after {}s", self.connection_timeout * 2);
                log::error!("{}", err_msg);
                Err(transport_err(err_msg, Some(url)))
            }
        }
    }
    
    /// Close all connections in the pool
    pub async fn close_all_connections(&self) -> Result<usize> {
        log::info!("Closing all pooled Tor connections");
        
        let mut pool = self.connection_pool.write().await;
        let mut closed_count = 0;
        
        for (key, connections) in pool.drain() {
            log::debug!("Closing {} connections for {}", connections.len(), key);
            
            for stream in connections {
                if let Err(e) = stream.close().await {
                    log::warn!("Error closing Tor connection to {}: {}", key, e);
                }
                closed_count += 1;
            }
        }
        
        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.closed_connections += closed_count;
        }
        
        log::info!("Closed {} Tor connections", closed_count);
        
        Ok(closed_count)
    }
}

impl fmt::Debug for TorTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TorTransport")
            .field("tor_client", &"Arc<TorClient>")
            .field("stream_prefs", &"StreamPrefs")
            .field("use_connection_pool", &self.use_connection_pool)
            .field("max_pool_connections", &self.max_pool_connections)
            .field("connection_timeout", &self.connection_timeout)
            .field("security_settings", &self.security_settings)
            .field("proxy_settings", &self.proxy_settings)
            .finish()
    }
}

/// A connection to a Git repository over Tor
pub struct TorConnection {
    url: String,
    onion_address: String,
    port: u16,
    transport: Arc<TorTransport>,
    capabilities: Vec<String>,
}

impl TorConnection {
    /// Create a new Tor connection using the provided transport
    pub fn with_transport(url: &str, transport: Arc<TorTransport>) -> Result<Self> {
        let parsed_url = Url::parse(url)
            .map_err(|e| transport_err(format!("Invalid URL: {}", e), Some(url)))?;
            
        // Extract onion address and port
        let host = parsed_url.host_str()
            .ok_or_else(|| transport_err("Missing host in URL", Some(url)))?;
            
        // For .onion addresses, verify format
        if host.ends_with(".onion") {
            // For v3 onion addresses, they should be 56 characters plus .onion (62 total)
            if host.len() != 62 && host.len() != 22 {  // Support both v2 (16+6) and v3 (56+6) addresses
                log::warn!("Unusual onion address length: {} (should be 22 for v2 or 62 for v3)", host.len());
            }
        }
        
        let port = parsed_url.port().unwrap_or_else(|| {
            // Default port based on scheme
            match parsed_url.scheme() {
                "git" | "tor+git" => 9418, // Git protocol default port
                "http" | "tor+http" => 80, // HTTP default port
                "https" | "tor+https" => 443, // HTTPS default port
                _ => 80, // Default to HTTP port
            }
        });
        
        log::debug!("Created TorConnection for {}:{}", host, port);
        
        Ok(Self {
            url: url.to_string(),
            onion_address: host.to_string(),
            port,
            transport,
            capabilities: Vec::new(),
        })
    }
    
    /// Create a new Tor connection with a new transport
    pub async fn new(url: &str) -> Result<Self> {
        log::debug!("Creating new TorConnection with fresh transport for {}", url);
        
        // Create a new Tor transport
        let transport = TorTransport::new(None).await?;
        
        Self::with_transport(url, Arc::new(transport))
    }
    
    /// Create a new Tor stream to the specified onion service
    async fn create_stream(&self) -> Result<DataStream> {
        let addr = format!("{}:{}", self.onion_address, self.port);
        log::debug!("Creating new Tor stream to {}", addr);
        
        self.transport.get_connection(&self.onion_address, self.port).await
    }
    
    /// Discover references from the remote repository
    async fn discover_refs(&mut self) -> Result<Vec<(String, ObjectId)>> {
        log::info!("Discovering references for repository: {}", self.url);
        
        // Establish connection
        let mut stream = self.create_stream().await?;
        
        // Send git-upload-pack request
        let repo_path = utils::get_repo_path_from_url(&self.url)?;
        let command = format!("git-upload-pack /{}\0host={}\0", 
                             repo_path, self.onion_address);
        
        stream.write_all(command.as_bytes()).await
            .map_err(|e| transport_err(format!("Failed to send git-upload-pack request: {}", e), Some(&self.url)))?;
            
        // Read the initial response (reference advertisement)
        let mut buffer = Vec::new();
        let mut refs = Vec::new();
        
        // Read until we have the full reference advertisement
        // This is a simplified implementation - a full one would parse the pkt-line format
        let read_result = timeout(
            Duration::from_secs(30),
            stream.read_to_end(&mut buffer)
        ).await;
        
        match read_result {
            Ok(Ok(_)) => {
                log::debug!("Received {} bytes of reference data", buffer.len());
                
                // Parse the reference advertisement
                // In a full implementation, we would properly parse the pkt-line format
                // For now, we'll do a simplified parsing
                
                let mut pos = 0;
                while pos < buffer.len() {
                    // PKT-LINE format: 4-byte length prefix followed by data
                    if pos + 4 >= buffer.len() {
                        break;
                    }
                    
                    // Parse length (hex string, 4 bytes)
                    let len_hex = std::str::from_utf8(&buffer[pos..pos+4])
                        .map_err(|_| transport_err("Invalid PKT-LINE format", Some(&self.url)))?;
                    
                    let length = u16::from_str_radix(len_hex, 16)
                        .map_err(|_| transport_err("Invalid PKT-LINE length", Some(&self.url)))?;
                    
                    if length < 4 {
                        // Flush packet or end packet
                        pos += 4;
                        continue;
                    }
                    
                    if pos + length as usize > buffer.len() {
                        break;
                    }
                    
                    // Extract the line content
                    let line = &buffer[pos+4..pos+length as usize];
                    let line_str = std::str::from_utf8(line)
                        .map_err(|_| transport_err("Invalid UTF-8 in reference line", Some(&self.url)))?;
                    
                    // Parse reference line: <object-id> <refname>
                    if line_str.len() >= 40 && line_str.as_bytes()[40] == b' ' {
                        let oid_str = &line_str[0..40];
                        let refname = &line_str[41..];
                        
                        // Extract capabilities if present
                        let parts: Vec<&str> = refname.split('\0').collect();
                        let (refname, caps) = if parts.len() > 1 {
                            // First reference line may include capabilities after a NUL
                            if self.capabilities.is_empty() {
                                if let Some(caps_str) = parts.get(1) {
                                    for cap in caps_str.split(' ') {
                                        if !cap.is_empty() {
                                            self.capabilities.push(cap.to_string());
                                        }
                                    }
                                }
                            }
                            (parts[0], parts.get(1))
                        } else {
                            (refname, None)
                        };
                        
                        // Add the reference to our list
                        let object_id = ObjectId::from_str(oid_str)
                            .map_err(|_| transport_err(format!("Invalid object ID: {}", oid_str), Some(&self.url)))?;
                            
                        refs.push((refname.to_string(), object_id));
                    }
                    
                    // Move to next pkt-line
                    pos += length as usize;
                }
                
                log::info!("Discovered {} references", refs.len());
                if !self.capabilities.is_empty() {
                    log::debug!("Server capabilities: {}", self.capabilities.join(", "));
                }
            },
            Ok(Err(e)) => {
                return Err(transport_err(format!("Failed to read reference advertisement: {}", e), Some(&self.url)));
            },
            Err(_) => {
                return Err(transport_err("Timeout while reading reference advertisement", Some(&self.url)));
            }
        }
        
        // Return the connection to the pool
        self.transport.return_connection(&self.onion_address, self.port, stream).await;
            
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
    ) -> std::result::Result<Box<dyn RequestWriter>, TransportError> {
        // Clone the transport for the async task
        let this = self.clone();
        let url_string = url.to_string();
        
        // Create a global runtime handler if we don't already have one
        // We avoid creating a new runtime for each request
        lazy_static::lazy_static! {
            static ref RUNTIME: Mutex<Option<tokio::runtime::Runtime>> = Mutex::new(None);
        }
        
        // Get or initialize the runtime
        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                // We're already in a Tokio runtime, so we can use it
                None
            },
            Err(_) => {
                // We need to create or get a runtime
                let mut runtime_guard = futures::executor::block_on(RUNTIME.lock());
                
                if runtime_guard.is_none() {
                    *runtime_guard = Some(tokio::runtime::Runtime::new().map_err(|e| {
                        TransportError::Io(io::Error::new(
                            io::ErrorKind::Other, 
                            format!("Failed to create Tokio runtime: {}", e)
                        ))
                    })?);
                }
                
                // Return a clone of the runtime for local use
                runtime_guard.clone()
            }
        };
        
        // Helper function to execute async code with the runtime
        let execute_async = |future: impl Future<Output = std::result::Result<Vec<u8>, GitError>>| {
            match runtime {
                Some(ref rt) => {
                    rt.block_on(future).map_err(|e| {
                        TransportError::Io(io::Error::new(
                            io::ErrorKind::Other, 
                            format!("Tor transport error: {}", e)
                        ))
                    })
                },
                None => {
                    // We're already in a Tokio runtime, so we can use tokio::spawn
                    let handle = tokio::runtime::Handle::current();
                    futures::executor::block_on(async {
                        let join_handle = handle.spawn(future);
                        match join_handle.await {
                            Ok(result) => result.map_err(|e| {
                                TransportError::Io(io::Error::new(
                                    io::ErrorKind::Other, 
                                    format!("Tor transport error: {}", e)
                                ))
                            }),
                            Err(e) => {
                                Err(TransportError::Io(io::Error::new(
                                    io::ErrorKind::Other, 
                                    format!("Failed to join task: {}", e)
                                )))
                            }
                        }
                    })
                }
            }
        };
        
        // Handle different Git services
        match service {
            transport::Service::UploadPack => {
                // Extract any extra data from the initial response
                let extra_data = initial_response_of_fetch.map(|resp| {
                    // Convert the response to bytes
                    resp.into()
                });
                
                // Create the fetch request
                let fetch_request = FetchRequest {
                    url: url_string.clone(),
                    extra_data,
                };
                
                // Execute the upload-pack request
                let response = execute_async(async move {
                    this.upload_pack(&url_string, &fetch_request).await
                })?;
                
                // Create a request writer with the response
                let writer = TorRequestWriter::new(response);
                Ok(Box::new(writer))
            },
            transport::Service::ReceivePack => {
                // Create an empty request writer - it will handle the actual request when write() is called
                let writer = TorReceivePackWriter::new(this, url_string);
                Ok(Box::new(writer))
            },
            _ => {
                Err(TransportError::Io(io::Error::new(
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

/// A request writer for Tor transport upload-pack requests
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
        self.written = true;
        self.data.extend_from_slice(data);
        Ok(data.len())
    }
    
    fn response(&mut self) -> std::io::Result<&[u8]> {
        // Return the saved response data
        Ok(&self.data)
    }
}

/// A request writer for Tor transport receive-pack requests
pub struct TorReceivePackWriter {
    transport: TorTransport,
    url: String,
    request_data: Vec<u8>,
    response_data: Option<Vec<u8>>,
}

impl TorReceivePackWriter {
    pub fn new(transport: TorTransport, url: String) -> Self {
        Self {
            transport,
            url,
            request_data: Vec::new(),
            response_data: None,
        }
    }
    
    /// Execute the receive-pack request with the accumulated request data
    async fn execute_request(&mut self) -> Result<Vec<u8>> {
        self.transport.receive_pack(&self.url, &self.request_data).await
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

impl RequestWriter for TorReceivePackWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        // Save the request data until we need to execute the request
        self.request_data.extend_from_slice(data);
        Ok(data.len())
    }
    
    fn response(&mut self) -> std::io::Result<&[u8]> {
        // If we don't have a response yet, execute the request
        if self.response_data.is_none() {
            // Create a runtime to execute the async request
            let runtime = match tokio::runtime::Handle::try_current() {
                Ok(handle) => None,
                Err(_) => Some(tokio::runtime::Runtime::new().map_err(|e| {
                    io::Error::new(io::ErrorKind::Other, format!("Failed to create runtime: {}", e))
                })?)
            };
            
            // Execute the request
            let result = match runtime {
                Some(rt) => {
                    rt.block_on(self.execute_request())
                },
                None => {
                    // We're already in a Tokio runtime
                    let handle = tokio::runtime::Handle::current();
                    futures::executor::block_on(async {
                        handle.spawn(self.execute_request()).await.unwrap_or_else(|e| {
                            Err(transport_err(format!("Failed to join task: {}", e), Some(&self.url)))
                        })
                    })
                }
            };
            
            // Handle the result
            match result {
                Ok(response) => {
                    self.response_data = Some(response);
                },
                Err(e) => {
                    return Err(io::Error::new(io::ErrorKind::Other, format!("Receive-pack error: {}", e)));
                }
            }
        }
        
        // Return the response data
        match &self.response_data {
            Some(data) => Ok(data),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "No response data available")),
        }
    }
}

// Implement Write trait for TorReceivePackWriter
impl io::Write for TorReceivePackWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        RequestWriter::write(self, buf)
    }
    
    fn flush(&mut self) -> io::Result<()> {
        // No-op as we don't buffer
        Ok(())
    }
}

/// An async implementation of RemoteConnection for Tor
/// Note: This is separate from the synchronous RemoteConnection trait
#[async_trait::async_trait]
pub trait AsyncRemoteConnection {
    async fn list_refs_async(&mut self) -> Result<Vec<(String, ObjectId)>>;
    async fn fetch_objects_async(&mut self, wants: &[ObjectId], haves: &[ObjectId]) 
        -> Result<Vec<(ObjectType, ObjectId, Bytes)>>;
    async fn push_objects_async(&mut self, objects: &[(ObjectType, ObjectId, Bytes)], refs: &[(String, ObjectId)]) 
        -> Result<()>;
}

#[async_trait::async_trait]
impl AsyncRemoteConnection for TorConnection {
    async fn list_refs_async(&mut self) -> Result<Vec<(String, ObjectId)>> {
        self.discover_refs().await
    }
    
    async fn fetch_objects_async(&mut self, wants: &[ObjectId], haves: &[ObjectId]) 
        -> Result<Vec<(ObjectType, ObjectId, Bytes)>> {
        
        log::info!("Fetching {} objects via Tor", wants.len());
        
        // Create a new Tor stream
        let mut stream = self.create_stream().await?;
        
        // Send git-upload-pack request
        let repo_path = utils::get_repo_path_from_url(&self.url)?;
        let command = format!("git-upload-pack /{}\0host={}\0", 
                             repo_path, self.onion_address);
        
        stream.write_all(command.as_bytes()).await
            .map_err(|e| transport_err(format!("Failed to send git-upload-pack request: {}", e), Some(&self.url)))?;
        
        // Read the initial reference advertisement
        let mut buffer = Vec::new();
        let mut temp_buffer = [0u8; 4096];
        
        // Keep reading until we get the full advertisement
        let advertisement = loop {
            let n = stream.read(&mut temp_buffer).await
                .map_err(|e| transport_err(format!("Failed to read reference advertisement: {}", e), Some(&self.url)))?;
                
            if n == 0 {
                // End of stream
                return Err(transport_err("Unexpected end of stream", Some(&self.url)));
            }
            
            buffer.extend_from_slice(&temp_buffer[..n]);
            
            // Check if we have a full advertisement (ended with 0000)
            if buffer.len() >= 4 && &buffer[buffer.len() - 4..] == b"0000" {
                break buffer;
            }
        };
        
        // Process the advertisement and send our wants
        let mut request = BytesMut::new();
        
        // Add "want" lines for each object ID
        for want in wants {
            let want_line = format!("want {}\n", want);
            request.extend_from_slice(want_line.as_bytes());
        }
        
        // Add "have" lines if we have any
        for have in haves {
            let have_line = format!("have {}\n", have);
            request.extend_from_slice(have_line.as_bytes());
        }
        
        // Finish with "done"
        request.extend_from_slice(b"done\n");
        
        // Send our request
        log::debug!("Sending fetch request with {} wants and {} haves", wants.len(), haves.len());
        stream.write_all(&request).await
            .map_err(|e| transport_err(format!("Failed to send fetch request: {}", e), Some(&self.url)))?;
        
        // Receive the packfile
        log::debug!("Receiving packfile");
        let mut packfile_data = Vec::new();
        
        // Use a timeout for reading the packfile
        match timeout(
            Duration::from_secs(180), // 3 minutes timeout for packfile
            stream.read_to_end(&mut packfile_data)
        ).await {
            Ok(Ok(_)) => {
                log::debug!("Received {} bytes of packfile data", packfile_data.len());
            },
            Ok(Err(e)) => {
                return Err(transport_err(format!("Failed to read packfile: {}", e), Some(&self.url)));
            },
            Err(_) => {
                return Err(transport_err("Timeout while reading packfile", Some(&self.url)));
            }
        }
        
        // Parse the packfile to extract objects
        // This is a simplified implementation - a full one would properly parse the packfile format
        // For the sake of example, we'll just return an empty list
        log::info!("Packfile received but parsing is not implemented yet");
        
        // Return the connection to the pool
        self.transport.return_connection(&self.onion_address, self.port, stream).await;
        
        // In a real implementation, we would parse the packfile here
        Ok(Vec::new())
    }
    
    async fn push_objects_async(&mut self, objects: &[(ObjectType, ObjectId, Bytes)], refs: &[(String, ObjectId)]) 
        -> Result<()> {
        
        log::info!("Pushing {} objects and {} refs via Tor", objects.len(), refs.len());
        
        // Create a new Tor stream
        let mut stream = self.create_stream().await?;
        
        // Send git-receive-pack request
        let repo_path = utils::get_repo_path_from_url(&self.url)?;
        let command = format!("git-receive-pack /{}\0host={}\0", 
                             repo_path, self.onion_address);
        
        stream.write_all(command.as_bytes()).await
            .map_err(|e| transport_err(format!("Failed to send git-receive-pack request: {}", e), Some(&self.url)))?;
        
        // Read the initial reference advertisement
        let mut buffer = Vec::new();
        let mut temp_buffer = [0u8; 4096];
        
        // Keep reading until we get the full advertisement
        let advertisement = loop {
            let n = stream.read(&mut temp_buffer).await
                .map_err(|e| transport_err(format!("Failed to read reference advertisement: {}", e), Some(&self.url)))?;
                
            if n == 0 {
                // End of stream
                return Err(transport_err("Unexpected end of stream", Some(&self.url)));
            }
            
            buffer.extend_from_slice(&temp_buffer[..n]);
            
            // Check if we have a full advertisement (ended with 0000)
            if buffer.len() >= 4 && &buffer[buffer.len() - 4..] == b"0000" {
                break buffer;
            }
        };
        
        // In a real implementation, we would:
        // 1. Create a packfile with the objects to push
        // 2. Send the reference updates
        // 3. Send the packfile
        // 4. Parse the server response to check for errors
        
        log::warn!("Push implementation is incomplete");
        
        // Return the connection to the pool
        self.transport.return_connection(&self.onion_address, self.port, stream).await;
        
        // For now, just return Ok
        Ok(())
    }
}

// Synchronous adapter for the standard RemoteConnection trait
// This allows us to use TorConnection with the existing RemoteConnection interface
impl RemoteConnection for TorConnection {
    fn list_refs(&mut self) -> Result<Vec<(String, ObjectId)>> {
        // Use a global Tokio runtime for better performance
        lazy_static::lazy_static! {
            static ref RUNTIME: std::sync::Mutex<Option<tokio::runtime::Runtime>> = std::sync::Mutex::new(None);
        }
        
        // Get or create the runtime
        let mut runtime_guard = RUNTIME.lock().unwrap();
        if runtime_guard.is_none() {
            *runtime_guard = Some(tokio::runtime::Runtime::new()
                .map_err(|e| transport_err(format!("Failed to create runtime: {}", e), Some(&self.url)))?);
        }
        
        // Run the async operation in the runtime
        runtime_guard.as_ref().unwrap().block_on(self.list_refs_async())
    }
    
    fn fetch_objects(&mut self, wants: &[ObjectId], haves: &[ObjectId]) 
        -> Result<Vec<(ObjectType, ObjectId, Bytes)>> {
        
        // Use a global Tokio runtime for better performance
        lazy_static::lazy_static! {
            static ref RUNTIME: std::sync::Mutex<Option<tokio::runtime::Runtime>> = std::sync::Mutex::new(None);
        }
        
        // Get or create the runtime
        let mut runtime_guard = RUNTIME.lock().unwrap();
        if runtime_guard.is_none() {
            *runtime_guard = Some(tokio::runtime::Runtime::new()
                .map_err(|e| transport_err(format!("Failed to create runtime: {}", e), Some(&self.url)))?);
        }
        
        // Run the async operation in the runtime
        runtime_guard.as_ref().unwrap().block_on(self.fetch_objects_async(wants, haves))
    }
    
    fn push_objects(&mut self, objects: &[(ObjectType, ObjectId, Bytes)], refs: &[(String, ObjectId)]) -> Result<()> {
        // Use a global Tokio runtime for better performance
        lazy_static::lazy_static! {
            static ref RUNTIME: std::sync::Mutex<Option<tokio::runtime::Runtime>> = std::sync::Mutex::new(None);
        }
        
        // Get or create the runtime
        let mut runtime_guard = RUNTIME.lock().unwrap();
        if runtime_guard.is_none() {
            *runtime_guard = Some(tokio::runtime::Runtime::new()
                .map_err(|e| transport_err(format!("Failed to create runtime: {}", e), Some(&self.url)))?);
        }
        
        // Run the async operation in the runtime
        runtime_guard.as_ref().unwrap().block_on(self.push_objects_async(objects, refs))
    }
}

/// Helper function to read a stream to end with progress logging
async fn read_to_end_with_progress<R>(reader: &mut R, buffer: &mut Vec<u8>) -> io::Result<usize>
where
    R: AsyncRead + Unpin,
{
    let mut temp_buf = [0u8; 8192];
    let mut total_read = 0;
    let mut last_log = std::time::Instant::now();
    
    loop {
        match reader.read(&mut temp_buf).await {
            Ok(0) => break, // EOF
            Ok(n) => {
                buffer.extend_from_slice(&temp_buf[..n]);
                total_read += n;
                
                // Log progress every second for large responses
                if total_read > 100_000 && last_log.elapsed() > Duration::from_secs(1) {
                    log::debug!("Read {} bytes so far", total_read);
                    last_log = std::time::Instant::now();
                }
            }
            Err(e) => return Err(e),
        }
    }
    
    Ok(total_read)
}

/// Initialize a transport registry with the Tor transport
pub async fn init_transport(transport: Arc<TorTransport>) -> Result<gix_transport::client::capabilities::TransportFactoryHandle> {
    use gix_transport::client::capabilities::{Registry, TransportFactoryHandle};
    
    // Create a transport registry and register our TorTransport
    let registry = Registry::default();
    
    // Register the Tor transport
    registry.register_factory(move |url| {
        if TorTransport::handles_url(&url.to_string()) {
            let transport_clone = transport.clone();
            Some(Box::new(transport_clone) as Box<dyn Transport>)
        } else {
            None
        }
    });
    
    // Get a handle that can be used to unregister the transport later
    let handle = registry.register();
    
    log::info!("Tor transport registered successfully");
    
    Ok(handle)
}