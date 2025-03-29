mod http;
mod tor;
mod gix_tor;
mod registry;

pub use http::HttpConnection;
pub use tor::{TorConnection, AsyncRemoteConnection};
pub use gix_tor::{TorTransport, TorGixConnection, TorTransportError, create_tor_transport};
pub use registry::{ArtiGitTransportRegistry, create_transport_registry};

use crate::core::{Result, ObjectId, ObjectType};

/// Common trait for all transport types
pub trait Transport {
    /// Get references from the remote
    fn list_refs(&mut self) -> Result<Vec<(String, ObjectId)>>;
    
    /// Fetch objects from the remote
    fn fetch(&mut self, wants: &[ObjectId], haves: &[ObjectId]) -> Result<Vec<(ObjectType, Vec<u8>)>>;
    
    /// Push objects to the remote
    fn push(&mut self, objects: &[(ObjectType, Vec<u8>)], refs: &[(String, ObjectId)]) -> Result<()>;
}

/// Factory for creating appropriate transport implementations based on URL
pub struct TransportFactory;

impl TransportFactory {
    /// Create a new transport based on the URL scheme
    pub fn create(url: &str) -> Result<Box<dyn Transport>> {
        if url.starts_with("http://") || url.starts_with("https://") {
            Ok(Box::new(http::HttpConnection::new(url)?))
        } else if url.contains(".onion") {
            Ok(Box::new(tor::TorConnection::new(url)?))
        } else {
            Err(crate::core::GitError::Transport(format!(
                "Unsupported URL scheme: {}", url
            )))
        }
    }
}