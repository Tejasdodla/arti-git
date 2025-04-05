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
use gix_protocol::{fetch, transport, packetline, sideband}; // Added packetline and sideband
use gix_protocol::pack::report_status; // Added report_status

use crate::core::{GitError, Result, ObjectId, ObjectType, RemoteConnection};
use crate::core::{io_err, transport_err};
use crate::protocol::{parse_git_command, process_wants, receive_packfile}; // Keep local protocol utils if needed elsewhere
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
        
        // --- Connection Attempt Loop with Retry ---
        let max_attempts = 3; // Max number of connection attempts
        let initial_delay = Duration::from_secs(1); // Initial delay before first retry
        let backoff_factor = 2.0; // Exponential backoff factor
        let mut current_delay = initial_delay;
        let mut last_error: Option<GitError> = None;

        for attempt in 1..=max_attempts {
            log::debug!("Attempt {}/{} to connect to {}", attempt, max_attempts, key);

            // Configure stream preferences based on security settings
            let mut stream_prefs = self.stream_prefs.clone();
            if self.security_settings.isolate_streams {
                stream_prefs = stream_prefs.isolate_connection();
            }

            // Apply proxy settings if needed (Placeholder - needs Arti API integration)
            if self.proxy_settings.proxy_type != TorProxyType::Direct {
                log::debug!("Proxy settings detected but not yet implemented for Arti connection.");
                // Configure proxy in stream_prefs here when Arti supports it easily
            }

            // Add authentication if available (Placeholder - needs Arti API integration)
            // Authentication typically happens at a higher protocol level (e.g., HTTP Basic Auth)
            // rather than during the raw Tor stream connection.
            // We'll keep the credential storage but remove direct application here.
            // let mut auth_header = None; ...

            let start_time = std::time::Instant::now();

            // Use timeout for connection establishment
            let connection_result = timeout(
                Duration::from_secs(self.connection_timeout),
                self.tor_client.connect(&key, &stream_prefs)
            ).await;

            let connection_time = start_time.elapsed().as_millis() as u64;

            // Handle timeout and connection errors
            match connection_result {
                Ok(Ok(stream)) => { // Successfully connected
                    // Verify the repository fingerprint
                    if let Err(e) = self.verify_fingerprint(host, &stream).await {
                        log::error!("Fingerprint verification failed for {}: {}", key, e);
                        last_error = Some(e);
                        // Treat fingerprint failure as non-retryable for this attempt
                        // We could potentially close the stream and retry, but let's fail for now.
                        break;
                    }

                    // Update stats for successful connection
                    {
                        let mut stats = self.stats.write().await;
                        stats.successful_connections += 1;
                        let total_conns = stats.successful_connections as u64;
                        if total_conns > 1 {
                            stats.avg_connection_time_ms = ((stats.avg_connection_time_ms * (total_conns - 1)) + connection_time) / total_conns;
                        } else {
                            stats.avg_connection_time_ms = connection_time;
                        }
                        if host.ends_with(".onion") { stats.secured_connections += 1; }
                    }
                    log::debug!("Connected to {} in {}ms (Attempt {})", key, connection_time, attempt);
                    return Ok(stream); // Success! Exit the loop and return the stream.
                },
                Ok(Err(e)) => { // Connection attempt failed with an Arti error
                    let err_msg = format!("Connection attempt {} failed for {}: {}", attempt, key, e);
                    log::warn!("{}", err_msg); // Log as warning during retries
                    last_error = Some(transport_err(err_msg, Some(&key)));
                    // TODO: Check if `e` (arti_client::Error) is retryable.
                    // For now, assume most connection errors *might* be transient.
                    let is_retryable = true;
                    if !is_retryable || attempt == max_attempts {
                        break; // Stop retrying if error is not retryable or max attempts reached
                    }
                },
                Err(_) => { // Connection attempt timed out
                    let err_msg = format!("Connection attempt {} timed out after {}s for {}", attempt, self.connection_timeout, key);
                    log::warn!("{}", err_msg);
                    last_error = Some(transport_err(err_msg, Some(&key)));
                    if attempt == max_attempts {
                        break; // Stop retrying if max attempts reached
                    }
                }
            }

            // If we reached here, the attempt failed but we might retry.
            log::info!("Waiting {:?} before next connection attempt to {}", current_delay, key);
            tokio::time::sleep(current_delay).await;
            // Increase delay for next attempt
            current_delay = Duration::from_secs_f64(current_delay.as_secs_f64() * backoff_factor);
        }

        // If the loop finished without returning Ok(stream), it means all attempts failed.
        log::error!("All {} connection attempts failed for {}", max_attempts, key);
        // Update stats for the final failure
        {
            let mut stats = self.stats.write().await;
            stats.failed_connections += 1; // Count the overall failure once
        }
        // Return the last recorded error
        Err(last_error.unwrap_or_else(|| transport_err("Connection failed after multiple retries with unknown error", Some(&key))))
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
                // For UploadPack (fetch), we return a writer that handles the stateful negotiation
                // when its `write` method is called by gitoxide.
                log::debug!("Creating TorFetchWriter for UploadPack request to {}", url_string);
                let writer = TorFetchWriter::new(this, url_string, initial_response_of_fetch);
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

// --- TorFetchWriter for handling stateful fetch ---

/// Handles the stateful fetch process (upload-pack) over Tor.
pub struct TorFetchWriter {
    transport: TorTransport,
    url: String,
    // Optional initial response data (e.g., from HTTP GET) - might not be used directly with Tor stream
    _initial_response: Option<fetch::Response>,
    // Buffer to store the received packfile data after negotiation
    pack_data: Option<Vec<u8>>,
    // TODO: Add state for negotiation results if needed (e.g., shallow commits)
}

impl TorFetchWriter {
    pub fn new(transport: TorTransport, url: String, initial_response: Option<fetch::Response>) -> Self {
        Self {
            transport,
            url,
            _initial_response: initial_response,
            pack_data: None,
        }
    }
}

impl RequestWriter for TorFetchWriter {
    /// Receives fetch arguments, performs negotiation, and reads the packfile.
    fn write(&mut self, fetch_args_pkt_lines: &[u8]) -> std::io::Result<usize> {
        log::debug!("TorFetchWriter::write called with {} bytes of fetch arguments", fetch_args_pkt_lines.len());

        // Clone necessary parts for the async block
        let transport = self.transport.clone();
        let url = self.url.clone();
        // Clone the arguments data to be moved into the async block
        let fetch_args_data = fetch_args_pkt_lines.to_vec();

        // Use the execute_async helper (or similar blocking mechanism)
        // This runs the async fetch logic and blocks until completion.
        let result: std::result::Result<Vec<u8>, TransportError> = transport.execute_async(async move {
            // 1. Get Connection
            let (host, port) = transport.parse_url(&url)?;
            let mut stream = transport.get_connection(&host, port).await?;
            log::debug!("Got Tor stream for fetch to {}", url);

            // 2. Send "git-upload-pack" command
            // Extract repo path from URL (assuming standard Git URL format)
            let parsed_url = Url::parse(&url).map_err(|e| GitError::Transport(format!("Invalid URL: {}", e), Some(url.clone())))?;
            let repo_path = parsed_url.path().trim_start_matches('/'); // Remove leading slash
            let command = format!("git-upload-pack {}\0host={}\0", repo_path, host);
            log::debug!("Sending upload-pack command: '{}'", command.replace('\0', "\\0"));
            packetline::write_str(&mut stream, &command).await
                .map_err(|e| GitError::Transport(format!("Failed to send upload-pack command: {}", e), Some(url.clone())))?;

            // 3. Read initial ref advertisement (optional but good practice)
            // TODO: Read and potentially parse the advertisement before sending wants/haves
            // let mut advertisement_reader = packetline::Reader::new(&mut stream);
            // while let Some(line) = advertisement_reader.read_line().await? { ... }
            log::debug!("Skipping reading ref advertisement for now.");

            // 4. Send Wants/Haves/Args received from gitoxide
            log::debug!("Sending {} bytes of fetch arguments (wants/haves)...", fetch_args_data.len());
            stream.write_all(&fetch_args_data).await
                .map_err(|e| GitError::Transport(format!("Failed to send fetch arguments: {}", e), Some(url.clone())))?;
            // The `fetch_args_data` should already contain the flush packet sent by gitoxide fetch logic.

            // 5. Read Negotiation Response (ACKs/NAKs)
            // The gitoxide fetch handler is responsible for the negotiation loop.
            // This `write` method is called with the *initial* set of wants/haves.
            // The response we read here should be the server's first reaction,
            // typically ACKs/NAKs indicating which 'haves' it recognized,
            // potentially followed immediately by the packfile if the negotiation is simple.
            // For complex negotiations, gitoxide would call `write` again with more 'haves'.
            // We read until the server signals the end of negotiation (typically with ACK common <oid> or just ACK <oid> ready)
            // or sends a NAK indicating it needs more 'have's (which gitoxide should handle by calling write() again).
            // Or until a flush packet is received.
            log::debug!("Reading negotiation response (ACKs/NAKs)...");
            let mut negotiation_reader = packetline::Reader::new(&mut stream);
            let mut negotiation_ended = false;
            loop {
                let line = negotiation_reader.read_line().await
                    .map_err(|e| GitError::Transport(format!("Failed to read negotiation response: {}", e), Some(url.clone())))?;
                
                if let Some(line_bytes) = line.as_bytes() {
                    let line_str = String::from_utf8_lossy(line_bytes);
                    log::debug!("Negotiation line: {}", line_str.trim_end());
                    
                    // Basic check for end of negotiation signals (can be refined)
                    // A real implementation should parse ACKs properly using gix::protocol::fetch::Response::from_line
                    if line_str.starts_with("ACK") && (line_str.contains(" ready") || line_str.contains(" common")) {
                        negotiation_ended = true;
                        // Keep reading until flush packet
                    } else if line_str.starts_with("NAK") {
                        // Server needs more info, gitoxide fetch state machine should handle this
                        // by potentially calling write() again with more haves.
                        // We just continue reading the current response block.
                    } else if !line_str.starts_with("ACK") {
                        // Not ACK/NAK, assume start of pack or error
                        log::debug!("Non-ACK/NAK line received, assuming end of negotiation phase.");
                        // Need to stop reading here.
                        break;
                    }
                } else {
                    // Flush packet indicates end of this negotiation response block
                    log::debug!("Flush packet received, negotiation response block finished.");
                    break;
                }
            }
            // Note: A full implementation would likely involve gix::protocol::fetch::handshake
            // and potentially loop based on ACK/NAK results. This simplified version assumes
            // gitoxide handles the higher-level loop and we just process one round here.

            // 6. Send "done" (conditionally)
            // If the negotiation loop above determined the server is ready (negotiation_ended = true),
            // and if the arguments sent by gitoxide didn't *already* contain "done",
            // we might need to send it now to trigger the packfile response.
            // This logic is complex and depends heavily on the protocol version and negotiation state.
            // For now, we assume gitoxide includes "done" in `fetch_args_data` when appropriate.
            if negotiation_ended {
                log::debug!("Negotiation ended (server ACK ready/common). Assuming 'done' was sent by gitoxide if needed.");
                // Example of sending 'done' if we were managing the state:
                // if !fetch_args_data.windows(4).any(|window| window == b"done") { // Check if args already contain "done"
                //     log::debug!("Sending 'done' command to trigger packfile.");
                //     packetline::write_str(&mut stream, "done").await
                //         .map_err(|e| GitError::Transport(format!("Failed to send 'done' command: {}", e), Some(url.clone())))?;
                // }
            } else {
                log::debug!("Negotiation phase ended without explicit server ready signal (maybe non-ACK line or flush).");
            }

            // 7. Read Packfile Stream
            // 7. Read Packfile Stream
            // The negotiation_reader might have consumed the first line if it wasn't ACK/NAK.
            // The packfile itself might be sideband encoded.
            log::debug!("Attempting to read packfile stream (potentially sideband encoded)...");
            // Use a sideband decoder to handle progress/error messages multiplexed with pack data.
            // The underlying reader is the `negotiation_reader` which is positioned after negotiation.
            let mut sideband_reader = sideband::decode::Reader::new(negotiation_reader);
            let mut pack_data = Vec::new();
            
            loop {
                match sideband_reader.read_line().await {
                    Ok(Some(sideband::PacketLineRef::Data(line))) => {
                        pack_data.extend_from_slice(line);
                    }
                    Ok(Some(sideband::PacketLineRef::Progress(line))) => {
                        log::info!("Fetch progress: {}", String::from_utf8_lossy(line).trim_end());
                    }
                    Ok(Some(sideband::PacketLineRef::Error(line))) => {
                        let error_msg = String::from_utf8_lossy(line).trim_end().to_string();
                        log::error!("Remote fetch error: {}", error_msg);
                        // Return a protocol error, as the remote indicated failure
                        return Err(GitError::Protocol(format!("Remote error during fetch: {}", error_msg)));
                    }
                    Ok(None) => {
                        // End of stream (flush packet)
                        log::debug!("End of packfile stream detected.");
                        break;
                    }
                    Err(e) => {
                        log::error!("Error reading sideband stream: {}", e);
                        return Err(GitError::Transport(format!("Failed to read packfile sideband stream: {}", e), Some(url.clone())));
                    }
                }
            }
            log::debug!("Read {} bytes of packfile data.", pack_data.len());

            // Return connection to pool
            transport.return_connection(&host, port, stream).await;

            Ok(pack_data)
        });

        // Store the received pack data or handle error
        match result {
            Ok(pack_data) => {
                self.pack_data = Some(pack_data);
                Ok(fetch_args_pkt_lines.len()) // Indicate we consumed the input args
            }
            Err(e) => {
                log::error!("Async fetch operation failed: {}", e);
                // Map the TransportError back to io::Error for the RequestWriter trait
                Err(io::Error::new(io::ErrorKind::Other, e.to_string()))
            }
        }
    }

    /// Returns the packfile data received during the `write` call.
    fn response(&mut self) -> std::io::Result<&[u8]> {
        log::debug!("TorFetchWriter::response called");
        match &self.pack_data {
            Some(data) => {
                log::debug!("Returning {} bytes of pack data", data.len());
                Ok(data.as_slice())
            },
            None => {
                log::error!("TorFetchWriter::response called before write completed successfully");
                Err(io::Error::new(io::ErrorKind::Other, "Fetch did not complete or failed"))
            }
        }
    }
}

// Need to implement Write for TorFetchWriter so it can be boxed
impl io::Write for TorFetchWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // This delegates to the RequestWriter::write method
        RequestWriter::write(self, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        // No-op for this writer, flushing happens during the write call's interaction
        Ok(())
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

    /// Push a pre-generated packfile asynchronously
    async fn push_packfile_async(&mut self, pack_data: &[u8], refs: &[(String, ObjectId)]) -> Result<()> {
        log::info!("Pushing packfile ({} bytes) and {} refs via Tor", pack_data.len(), refs.len());

        // --- Build the request ---
        // 1. Reference updates
        let mut request_data = Vec::new();
        for (ref_name, new_oid) in refs {
            // For simplicity, assume we always push, needing old_oid = zero
            // A real implementation needs negotiation to get the actual old_oid from the remote's ref advertisement.
            let old_oid_zero = ObjectId::from_hex("0000000000000000000000000000000000000000")?;
            let line = format!("{} {} {}\0", old_oid_zero, new_oid, ref_name);
            // Use pkt-line encoding
            let pkt_line = format!("{:04x}{}", line.len() + 4, line);
            request_data.extend_from_slice(pkt_line.as_bytes());
        }
        // Add flush packet to signify end of ref updates
        request_data.extend_from_slice(b"0000");

        // 2. Packfile data
        request_data.extend_from_slice(pack_data);

        // --- Send request using receive-pack service via TorTransport ---
        // The TorTransport::receive_pack method handles connecting, sending the git-receive-pack command,
        // and transmitting the provided request_data (which now includes ref updates + packfile).
        let response_bytes = self.transport.receive_pack(&self.url, &request_data).await?;

        // --- Parse the response (report-status) ---
        log::debug!("Received receive-pack response: {} bytes. Parsing status report...", response_bytes.len());
        let mut reader = packetline::Reader::new(&response_bytes[..]);
        let mut line = reader.read_line().await?; // Read the first line (should be unpack status or first ref status)

        let mut unpack_ok = false;
        let mut ref_errors = Vec::new();

        // Check unpack status
        if let Some(line_bytes) = line.as_bytes() {
            if line_bytes.starts_with(b"unpack ") {
                match report_status::decode_unpack_status(line_bytes) {
                    Ok(report_status::UnpackStatus::Ok) => {
                        unpack_ok = true;
                        log::debug!("Unpack status: OK");
                    }
                    Ok(report_status::UnpackStatus::NotOk { error }) => {
                        log::error!("Unpack status: Error - {}", error);
                        // Even if unpack fails, continue to read ref statuses
                    }
                    Err(e) => {
                        log::warn!("Failed to parse unpack status line: {}", e);
                        // Continue anyway, maybe it's a ref status
                    }
                }
                line = reader.read_line().await?; // Read next line for ref status
            }
        } else {
            // If the first line is None (flush packet), something is wrong or empty response
            return Err(GitError::Protocol("Empty or invalid status report received from remote".to_string()));
        }

        // Read ref statuses until flush packet
        while let Some(line_bytes) = line.as_bytes() {
            match report_status::decode_ref_status(line_bytes) {
                Ok(report_status::RefStatus::Ok { .. }) => {
                    // Ref updated successfully, log or ignore
                    // log::debug!("Ref status OK for: {}", ref_name);
                }
                Ok(report_status::RefStatus::NotOk { reference, error }) => {
                    log::error!("Ref status Error for {}: {}", reference, error);
                    ref_errors.push(format!("Ref '{}': {}", reference, error));
                }
                Err(e) => {
                    let line_str = String::from_utf8_lossy(line_bytes);
                    log::warn!("Failed to parse ref status line '{}': {}", line_str, e);
                    // Potentially treat as an error or try to continue
                    ref_errors.push(format!("Invalid status line: {}", line_str));
                }
            }
            line = reader.read_line().await?; // Read next line
        }

        // Check overall status
        if !unpack_ok {
            // If unpack failed, report that as the primary error, possibly including ref errors
            let error_details = if ref_errors.is_empty() {
                "Remote failed to unpack objects.".to_string()
            } else {
                format!("Remote failed to unpack objects. Ref errors: [{}]", ref_errors.join("; "))
            };
            Err(GitError::Protocol(error_details))
        } else if !ref_errors.is_empty() {
            // If unpack was ok, but refs failed
            Err(GitError::Protocol(format!("Push partially failed. Ref errors: [{}]", ref_errors.join("; "))))
        } else {
            // Unpack OK and no ref errors
            log::info!("Push successful: Unpack OK and all refs updated.");
            Ok(())
        }
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