/// Git LFS Server implementation
/// 
/// This implements the server-side of the Git LFS API
/// https://github.com/git-lfs/git-lfs/blob/main/docs/api/batch.md
use std::sync::Arc;
use std::path::{Path, PathBuf};
use std::collections::HashMap;

use serde::{Serialize, Deserialize};
use tokio::io::AsyncWriteExt;
use hyper::{Body, Request, Response, StatusCode};
use hyper::header::{HeaderValue, CONTENT_TYPE};
use url::Url;

use crate::core::{GitError, Result};
use super::{LfsClient, LfsStorage, LfsObjectId, LfsPointer};

/// The LFS batch request
#[derive(Debug, Deserialize)]
struct BatchRequest {
    operation: String,
    transfers: Option<Vec<String>>,
    #[serde(rename = "ref")]
    reference: Option<BatchReference>,
    objects: Vec<BatchRequestObject>,
}

/// Git reference information
#[derive(Debug, Deserialize)]
struct BatchReference {
    name: String,
}

/// Object in a batch request
#[derive(Debug, Deserialize)]
struct BatchRequestObject {
    oid: String,
    size: u64,
}

/// The LFS batch response
#[derive(Debug, Serialize)]
struct BatchResponse {
    transfer: String,
    objects: Vec<BatchResponseObject>,
}

/// Object in a batch response
#[derive(Debug, Serialize)]
struct BatchResponseObject {
    oid: String,
    size: u64,
    authenticated: Option<bool>,
    actions: Option<HashMap<String, BatchObjectAction>>,
    error: Option<BatchObjectError>,
}

/// Action for an object
#[derive(Debug, Serialize)]
struct BatchObjectAction {
    href: String,
    #[serde(rename = "header", skip_serializing_if = "Option::is_none")]
    headers: Option<HashMap<String, String>>,
    expires_in: Option<u64>,
}

/// Error for an object
#[derive(Debug, Serialize)]
struct BatchObjectError {
    code: u32,
    message: String,
}

pub struct LfsServer {
    /// The LFS client
    client: Arc<LfsClient>,
    
    /// The LFS storage backend
    storage: Arc<LfsStorage>,
    
    /// Base URL for this server
    base_url: String,
}

impl LfsServer {
    /// Create a new LFS server
    pub fn new(client: Arc<LfsClient>, storage: Arc<LfsStorage>, base_url: &str) -> Self {
        Self {
            client,
            storage,
            base_url: base_url.to_string(),
        }
    }
    
    /// Handle an LFS API request
    pub async fn handle_request(&self, req: Request<Body>) -> Result<Response<Body>> {
        let path = req.uri().path();
        
        match (req.method().as_str(), path) {
            ("POST", "/objects/batch") => self.handle_batch(req).await,
            ("GET", path) if path.starts_with("/objects/") => {
                let oid = path.strip_prefix("/objects/").unwrap_or("");
                self.handle_download(oid).await
            },
            ("PUT", path) if path.starts_with("/objects/") => {
                let oid = path.strip_prefix("/objects/").unwrap_or("");
                self.handle_upload(req, oid).await
            },
            _ => {
                Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("Not found"))
                    .unwrap())
            }
        }
    }
    
    /// Handle a batch request
    async fn handle_batch(&self, req: Request<Body>) -> Result<Response<Body>> {
        // Extract the JSON body
        let body_bytes = hyper::body::to_bytes(req.into_body())
            .await
            .map_err(|e| GitError::LfsError(format!("Failed to read request body: {}", e)))?;
            
        // Parse the batch request
        let batch_request: BatchRequest = serde_json::from_slice(&body_bytes)
            .map_err(|e| GitError::LfsError(format!("Failed to parse batch request: {}", e)))?;
            
        // Process the request based on operation type
        let response = match batch_request.operation.as_str() {
            "download" => self.process_download_batch(batch_request).await?,
            "upload" => self.process_upload_batch(batch_request).await?,
            _ => {
                return Ok(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Body::from("Unsupported operation"))
                    .unwrap());
            }
        };
        
        // Serialize and return the response
        let json_response = serde_json::to_string(&response)
            .map_err(|e| GitError::LfsError(format!("Failed to serialize batch response: {}", e)))?;
            
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/vnd.git-lfs+json"))
            .body(Body::from(json_response))
            .unwrap())
    }
    
    /// Process a download batch request
    async fn process_download_batch(&self, request: BatchRequest) -> Result<BatchResponse> {
        let mut response_objects = Vec::new();
        
        for obj in request.objects {
            let id = LfsObjectId::new(&obj.oid);
            
            // Check if the object exists in our storage
            let mut response_object = BatchResponseObject {
                oid: obj.oid,
                size: obj.size,
                authenticated: Some(true),
                actions: None,
                error: None,
            };
            
            if self.storage.has_object(&id).await {
                // Create download action
                let mut actions = HashMap::new();
                actions.insert("download".to_string(), BatchObjectAction {
                    href: format!("{}/objects/{}", self.base_url, id.as_str()),
                    headers: None,
                    expires_in: Some(86400), // 24 hours
                });
                
                response_object.actions = Some(actions);
            } else {
                // Object not found
                response_object.error = Some(BatchObjectError {
                    code: 404,
                    message: "Object not found".to_string(),
                });
            }
            
            response_objects.push(response_object);
        }
        
        Ok(BatchResponse {
            transfer: "basic".to_string(),
            objects: response_objects,
        })
    }
    
    /// Process an upload batch request
    async fn process_upload_batch(&self, request: BatchRequest) -> Result<BatchResponse> {
        let mut response_objects = Vec::new();
        
        for obj in request.objects {
            let id = LfsObjectId::new(&obj.oid);
            
            // Create response object
            let mut response_object = BatchResponseObject {
                oid: obj.oid,
                size: obj.size,
                authenticated: Some(true),
                actions: None,
                error: None,
            };
            
            // Check if the object already exists
            if self.storage.has_object(&id).await {
                // No need to upload, object already exists
                response_objects.push(response_object);
                continue;
            }
            
            // Create upload action
            let mut actions = HashMap::new();
            actions.insert("upload".to_string(), BatchObjectAction {
                href: format!("{}/objects/{}", self.base_url, id.as_str()),
                headers: None,
                expires_in: Some(86400), // 24 hours
            });
            
            response_object.actions = Some(actions);
            response_objects.push(response_object);
        }
        
        Ok(BatchResponse {
            transfer: "basic".to_string(),
            objects: response_objects,
        })
    }
    
    /// Handle an object download request
    async fn handle_download(&self, oid: &str) -> Result<Response<Body>> {
        let id = LfsObjectId::new(oid);
        
        // Check if the object exists
        if !self.storage.has_object(&id).await {
            return Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("Object not found"))
                .unwrap());
        }
        
        // Get the object data
        let data = self.storage.get_object_bytes(&id).await?;
        
        // Return the object data
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"))
            .body(Body::from(data))
            .unwrap())
    }
    
    /// Handle an object upload request
    async fn handle_upload(&self, req: Request<Body>, oid: &str) -> Result<Response<Body>> {
        let id = LfsObjectId::new(oid);
        
        // Read the request body
        let body_bytes = hyper::body::to_bytes(req.into_body())
            .await
            .map_err(|e| GitError::LfsError(format!("Failed to read request body: {}", e)))?;
            
        // Store the object
        self.storage.store_object(&id, &body_bytes).await?;
        
        // If IPFS is enabled, store the object there too
        if self.client.config().use_ipfs {
            if let Ok(cid) = self.client.upload_to_ipfs(&body_bytes).await {
                // Create a pointer with the IPFS CID
                let mut pointer = LfsPointer::new(oid, body_bytes.len() as u64);
                pointer.set_ipfs_cid(&cid);
                
                // TODO: Store the pointer mapping in a database
                println!("Stored LFS object {} in IPFS with CID: {}", oid, cid);
            }
        }
        
        // Return success
        Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(""))
            .unwrap())
    }
    
    /// Start the LFS server on a given address
    pub async fn start(&self, addr: &str) -> Result<()> {
        println!("Starting LFS server on {}", addr);
        println!("Using base URL: {}", self.base_url);
        
        // Create a new HTTP service
        let service = hyper::service::make_service_fn(move |_conn| {
            let server_clone = Arc::clone(&self);
            async move {
                Ok::<_, hyper::Error>(hyper::service::service_fn(move |req| {
                    let server = Arc::clone(&server_clone);
                    async move {
                        server.handle_request(req).await.map_err(|e| {
                            hyper::Error::new(std::io::Error::new(
                                std::io::ErrorKind::Other, 
                                format!("LFS error: {}", e)
                            ))
                        })
                    }
                }))
            }
        });
        
        // Parse the address
        let addr = addr.parse()
            .map_err(|e| GitError::LfsError(format!("Invalid address {}: {}", addr, e)))?;
        
        // Create the HTTP server
        let server = hyper::Server::bind(&addr)
            .serve(service);
            
        println!("LFS server started successfully on {}", addr);
        
        // Run the server
        server.await
            .map_err(|e| GitError::LfsError(format!("HTTP server error: {}", e)))?;
        
        Ok(())
    }
}