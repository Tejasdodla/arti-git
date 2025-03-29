use std::sync::{Arc, Mutex};
use std::rc::Rc;
use std::collections::HashMap;
use std::io;

use gix_transport::client;
use gix_transport::{client::Transport, client::capabilities};
use gix_url::Url;
use gix_protocol::transport;

use crate::core::{GitError, Result};
use crate::transport::TorTransport;
use crate::utils;

/// A transport registry that handles both standard Git transports and our custom Tor transport
pub struct ArtiGitTransportRegistry {
    tor_transport: Arc<TorTransport>,
    standard_registry: client::Registry,
    custom_schemes: Arc<Mutex<HashMap<String, Arc<TorTransport>>>>,
}

impl ArtiGitTransportRegistry {
    /// Create a new transport registry with the given TorTransport
    pub fn new(tor_transport: Arc<TorTransport>) -> Self {
        // Start with the default registry
        let standard_registry = client::Registry::default();
        
        Self {
            tor_transport,
            standard_registry,
            custom_schemes: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    
    /// Register custom URL schemes with gitoxide
    pub fn register_schemes() -> Result<()> {
        // Register tor+http, tor+https, tor+git schemes
        for scheme in &["tor+http", "tor+https", "tor+git"] {
            match gix_url::Scheme::register(scheme) {
                Ok(_) => {},
                Err(e) => {
                    // It's okay if the scheme is already registered
                    if !e.to_string().contains("already registered") {
                        return Err(GitError::Transport(format!(
                            "Failed to register URL scheme '{}': {}", scheme, e
                        )));
                    }
                }
            }
        }
        
        Ok(())
    }
    
    /// Register this transport registry with gitoxide
    pub fn register(&self) -> capabilities::TransportFactoryHandle {
        capabilities::register(self)
    }
}

// Implement the new Transport trait from gitoxide
impl client::Transport for ArtiGitTransportRegistry {
    fn request(
        &self, 
        url: &Url, 
        service: transport::Service, 
        args: Vec<transport::client::Argument<'_>>,
        initial_response_of_fetch: Option<gix_protocol::fetch::Response>
    ) -> std::result::Result<Box<dyn client::RequestWriter>, client::Error> {
        // Check if this is a Tor URL
        if url.scheme().starts_with("tor+") || utils::is_onion_address(url.as_str()) {
            return self.tor_transport.request(url, service, args, initial_response_of_fetch);
        }
        
        // Fall back to standard transport
        match self.standard_registry.transport_for(url) {
            Ok(transport) => transport.request(url, service, args, initial_response_of_fetch),
            Err(e) => Err(e),
        }
    }
    
    fn supports_url(url: &Url) -> bool {
        if url.scheme().starts_with("tor+") || utils::is_onion_address(url.as_str()) {
            return true;
        }
        
        // Fall back to standard transport mechanism
        client::Registry::default().supports_any(url)
    }
}

/// Factory function for creating ArtiGitTransportRegistry instances
/// that follow the Transport trait definition
impl client::TransportFactory for ArtiGitTransportRegistry {
    fn factory(&self, url: &Url) -> std::result::Result<Box<dyn Transport>, client::Error> {
        if url.scheme().starts_with("tor+") || utils::is_onion_address(url.as_str()) {
            // For Tor URLs, use our TorTransport
            Ok(Box::new(self.tor_transport.clone()))
        } else {
            // For other URLs, use standard transports
            self.standard_registry.factory(url)
        }
    }
    
    fn supports_any(&self, url: &Url) -> bool {
        url.scheme().starts_with("tor+") || utils::is_onion_address(url.as_str()) || 
        self.standard_registry.supports_any(url)
    }
    
    fn supports_protocol(&self, protocol: &str) -> bool {
        protocol.starts_with("tor+") || self.standard_registry.supports_protocol(protocol)
    }
}

/// Register URL schemes and create a transport registry
pub async fn create_transport_registry(tor_transport: Arc<TorTransport>) -> Result<ArtiGitTransportRegistry> {
    // Register custom schemes
    ArtiGitTransportRegistry::register_schemes()?;
    
    // Create and return the registry
    Ok(ArtiGitTransportRegistry::new(tor_transport))
}

/// Custom version of TransportFactoryHandle 
/// that provides better lifetime management for our registry
pub struct ArtiGitTransportFactoryHandle {
    registry: Arc<ArtiGitTransportRegistry>,
    _handle: capabilities::TransportFactoryHandle,
}

impl ArtiGitTransportFactoryHandle {
    /// Create a new ArtiGitTransportFactoryHandle
    pub fn new(registry: Arc<ArtiGitTransportRegistry>) -> Self {
        let handle = capabilities::register(&*registry);
        Self {
            registry,
            _handle: handle,
        }
    }
    
    /// Get a reference to the underlying registry
    pub fn registry(&self) -> &ArtiGitTransportRegistry {
        &self.registry
    }
}

/// Initialize the transport system with a TorTransport
pub async fn init_transport(tor_transport: Arc<TorTransport>) -> Result<ArtiGitTransportFactoryHandle> {
    // Register URL schemes
    ArtiGitTransportRegistry::register_schemes()?;
    
    // Create registry
    let registry = Arc::new(ArtiGitTransportRegistry::new(tor_transport));
    
    // Create and return the handle that keeps the registry alive
    Ok(ArtiGitTransportFactoryHandle::new(registry))
}