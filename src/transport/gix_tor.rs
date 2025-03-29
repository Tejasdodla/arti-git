use std::io::{self, Read, Write};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use arti_client::{DataStream, TorClient, TorClientConfig, StreamPrefs};
use bytes::Bytes;
use futures::ready;
use gix_transport::{client, Transport};
use gix_url::Url;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tor_rtcompat::{Runtime, PreferredRuntime};

use crate::core::Result as ArtiGitResult;

/// Errors specific to Tor transport
#[derive(Error, Debug)]
pub enum TorTransportError {
    #[error("Git transport error: {0}")]
    Git(#[from] gix_transport::client::Error),
    
    #[error("Arti error: {0}")]
    Arti(#[from] arti_client::Error),
    
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Invalid URL: {0}")]
    Url(String),
    
    #[error("Connection error: {0}")]
    Connection(String),
    
    #[error("Not initialized: {0}")]
    NotInitialized(String),
}

/// A wrapper around DataStream that implements AsyncRead and AsyncWrite
pub struct TorStream {
    stream: DataStream,
}

impl TorStream {
    pub fn new(stream: DataStream) -> Self {
        Self { stream }
    }
}

impl AsyncRead for TorStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for TorStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// An adapter that allows DataStream to be used with synchronous I/O
pub struct SyncTorStream {
    stream: TorStream,
    runtime: PreferredRuntime,
}

impl SyncTorStream {
    pub fn new(stream: DataStream, runtime: PreferredRuntime) -> Self {
        Self {
            stream: TorStream::new(stream),
            runtime,
        }
    }
}

impl Read for SyncTorStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut read_buf = tokio::io::ReadBuf::new(buf);
        
        // Use runtime to perform the async read synchronously
        self.runtime.block_on(async {
            Pin::new(&mut self.stream).poll_read(
                &mut Context::from_waker(futures::task::noop_waker_ref()),
                &mut read_buf,
            )
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Async read error: {:?}", e)))?;
            
            Ok(read_buf.filled().len())
        })
    }
}

impl Write for SyncTorStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Use runtime to perform the async write synchronously
        self.runtime.block_on(async {
            Pin::new(&mut self.stream).write(buf).await
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        // Use runtime to perform the async flush synchronously
        self.runtime.block_on(async {
            Pin::new(&mut self.stream).flush().await
        })
    }
}

/// A connection to a Git repository via Tor for gitoxide
pub struct TorGixConnection {
    stream: SyncTorStream,
    _url: Url,
}

impl client::Connection for TorGixConnection {
    fn handshake(&mut self) -> std::result::Result<client::SetServiceResponse, client::Error> {
        use gix_packetline as pkt;
        use std::io::{BufReader, BufRead};
        
        // Standard Git protocol v1 handshake
        // Send the git-upload-pack command
        let command = format!("git-upload-pack /\0host={}\0", self._url.host().unwrap_or("localhost"));
        pkt::WriteMode::Binary.to_write()
            .write_all(&mut self.stream, command.as_bytes())
            .map_err(|e| client::Error::from(e))?;
        
        // Read and parse the response
        let mut rd = BufReader::new(&mut self.stream);
        let mut line = String::new();
        let mut capabilities = Vec::new();
        let mut refs = Vec::new();
        
        // Parse first line which contains capabilities
        if let Ok(n) = rd.read_line(&mut line) {
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Unexpected EOF during handshake"
                ).into());
            }
            
            // Strip packet length prefix
            if line.len() > 4 {
                let line_content = &line[4..];
                
                // Extract capabilities from first line
                if let Some(cap_start) = line_content.find('\0') {
                    // Everything after the null byte is a capability
                    let caps = &line_content[cap_start + 1..].trim_end();
                    capabilities = caps.split(' ').map(|s| s.to_string()).collect();
                    
                    // Parse ref information (format: "<sha> <ref-name>\0<capabilities>")
                    let ref_part = &line_content[..cap_start];
                    if let Some((oid, name)) = ref_part.split_once(' ') {
                        refs.push((name.to_string(), oid.to_string()));
                    }
                }
            }
        }
        
        // Read additional refs
        line.clear();
        while let Ok(n) = rd.read_line(&mut line) {
            if n == 0 || line.starts_with("0000") {
                break; // End of packet or flush packet
            }
            
            // Remove packet length and trailing newline
            if line.len() > 4 {
                let line_content = &line[4..line.len() - 1]; // -1 for newline
                
                if let Some((oid, name)) = line_content.split_once(' ') {
                    refs.push((name.to_string(), oid.to_string()));
                }
            }
            
            line.clear();
        }
        
        Ok(client::SetServiceResponse {
            service: "git-upload-pack".into(),
            refs,
            capabilities,
        })
    }

    fn request(
        &mut self,
        write_mode: client::WriteMode,
        on_into_read: client::MessageKind,
    ) -> std::result::Result<client::ResponseBuilder, client::Error> {
        use gix_packetline::WriteMode;
        use std::io::BufReader;
        
        // Convert our write mode to packetline write mode
        let write_mode = match write_mode {
            client::WriteMode::Binary => WriteMode::Binary,
            client::WriteMode::Text => WriteMode::Text,
        };
        
        // Create a packetline writer with our stream
        let writer = write_mode.to_write();
        
        // Create a response builder that will read from our stream when written to
        let reader = BufReader::new(&mut self.stream);
        let message_kind = match on_into_read {
            client::MessageKind::Flush => gix_protocol::MessageKind::Flush,
            client::MessageKind::Delimiter => gix_protocol::MessageKind::Delimiter,
            client::MessageKind::Response => gix_protocol::MessageKind::Response,
        };
        
        Ok(client::ResponseBuilder::new_from_buffered_read(
            reader, 
            writer,
            message_kind,
        ))
    }
}

/// A Tor transport for Git using gitoxide and Arti
pub struct TorTransport {
    client: Arc<TorClient<PreferredRuntime>>,
    runtime: PreferredRuntime,
}

impl TorTransport {
    /// Create a new Tor transport with an existing Tor client
    pub async fn new(tor_client: Option<Arc<TorClient<PreferredRuntime>>>) -> Result<Self, TorTransportError> {
        let runtime = PreferredRuntime::create()
            .map_err(|e| TorTransportError::Connection(format!("Failed to create runtime: {}", e)))?;
            
        // If client is provided, use it, otherwise create a new one
        let client = match tor_client {
            Some(client) => client,
            None => {
                let config = TorClientConfig::default();
                let client = TorClient::create_bootstrapped(runtime.clone(), config)
                    .await
                    .map_err(|e| TorTransportError::Arti(e))?;
                Arc::new(client)
            }
        };
            
        Ok(Self {
            client,
            runtime,
        })
    }
}

impl Transport for TorTransport {
    fn connect(&self, url: &Url) -> std::result::Result<Box<dyn client::Connection>, gix_transport::client::Error> {
        // Extract host and port from the URL
        let host = url.host().ok_or_else(|| {
            gix_transport::client::Error::from(io::Error::new(
                io::ErrorKind::InvalidInput,
                "No host in URL",
            ))
        })?;
        
        let port = url.port().unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
        
        // Create a Tor address string
        let addr = format!("{}:{}", host, port);
        
        // Configure stream preferences
        let prefs = StreamPrefs::default();
        
        // Use runtime to perform the async connect synchronously
        let stream = self.runtime.block_on(async {
            self.client.connect(&addr, &prefs)
                .await
                .map_err(|e| io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("Failed to connect via Tor: {}", e),
                ))
        })?;
        
        // Wrap the DataStream in our adapter
        let sync_stream = SyncTorStream::new(stream, self.runtime.clone());
        
        // Create and return the connection
        Ok(Box::new(TorGixConnection {
            stream: sync_stream,
            _url: url.clone(),
        }))
    }
}

/// Factory function to create a Tor transport
pub async fn create_tor_transport() -> ArtiGitResult<TorTransport> {
    TorTransport::new(None)
        .await
        .map_err(|e| crate::core::GitError::Transport(format!("Failed to create Tor transport: {}", e)))
}