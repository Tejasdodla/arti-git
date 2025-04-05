mod http;
mod tor;
mod gix_tor;
mod registry;

pub use http::HttpConnection;
pub use tor::{TorConnection, AsyncRemoteConnection};
pub use gix_tor::{TorTransport, TorGixConnection, TorTransportError, create_tor_transport};
pub use registry::{ArtiGitTransportRegistry, create_transport_registry};

use crate::core::Result; // Keep Result if used elsewhere, remove ObjectId, ObjectType if not
use std::sync::Arc;
use gix_transport::client::Transport as GixTransport; // Alias gitoxide's trait

/// Registers custom transports with gitoxide.
/// Should be called once at application startup.
pub async fn register_transports() -> Result<()> {
    // Register Tor transport for .onion addresses
    // Use the existing create_tor_transport function from gix_tor.rs
    let tor_transport = Arc::new(create_tor_transport(None).await?);

    // Define the condition for using this transport
    let tor_condition = |url: &gix_url::Url| -> bool {
        url.host().map_or(false, |host| host.ends_with(".onion"))
    };

    // Register the transport with the condition
    // This function overrides gitoxide's default transport resolution.
    gix_transport::client::set_required_transport_override(move |url, _remote_name| {
        if tor_condition(url) {
            Some(tor_transport.clone() as Arc<dyn GixTransport + Send + Sync>)
        } else {
            None // Let gitoxide handle other protocols (like file://, http://)
        }
    });

    log::info!("Registered Tor transport for .onion addresses.");

    // TODO: Register other custom transports if needed (e.g., IPFS)

    Ok(())
}