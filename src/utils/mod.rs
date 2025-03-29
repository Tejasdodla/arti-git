use std::path::{Path, PathBuf};
use std::fs;

use crate::core::{GitError, Result};

/// Get the absolute path from a potentially relative path
pub fn absolute_path(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|dir| dir.join(path))
            .map_err(|e| GitError::IO(format!("Failed to get current directory: {}", e)))
    }
}

/// Ensure a directory exists, creating it if necessary
pub fn ensure_dir_exists(dir: impl AsRef<Path>) -> Result<()> {
    let dir = dir.as_ref();
    if !dir.exists() {
        fs::create_dir_all(dir)
            .map_err(|e| GitError::IO(format!("Failed to create directory {}: {}", dir.display(), e)))?;
    } else if !dir.is_dir() {
        return Err(GitError::IO(format!("Path exists but is not a directory: {}", dir.display())));
    }
    Ok(())
}

/// Check if a path is within a given directory
pub fn is_path_within(path: impl AsRef<Path>, parent: impl AsRef<Path>) -> Result<bool> {
    let path = absolute_path(path)?;
    let parent = absolute_path(parent)?;
    
    Ok(path.starts_with(parent))
}

/// Get a URL's host name and port
pub fn parse_host_port(url: &str) -> Result<(String, u16)> {
    let url = url::Url::parse(url)
        .map_err(|e| GitError::InvalidArgument(format!("Invalid URL: {}", e)))?;
        
    let host = url.host_str()
        .ok_or_else(|| GitError::InvalidArgument("Missing host in URL".to_string()))?
        .to_string();
        
    let port = url.port().unwrap_or_else(|| 
        if url.scheme() == "https" { 443 }
        else if url.scheme() == "http" { 80 }
        else if url.scheme() == "git" { 9418 }
        else { 80 }
    );
    
    Ok((host, port))
}

/// Check if a URL is an onion address
pub fn is_onion_address(url: &str) -> bool {
    let url = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return false,
    };
    
    match url.host_str() {
        Some(host) => host.ends_with(".onion"),
        None => false,
    }
}

/// Format a repository URL for display (hiding sensitive info)
pub fn format_repo_url_safe(url: &str) -> String {
    if let Ok(mut parsed) = url::Url::parse(url) {
        // Remove username and password if present
        if parsed.username() != "" || parsed.password().is_some() {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            return parsed.to_string();
        }
    }
    
    // If parsing fails or no credentials, return the original URL
    url.to_string()
}

/// Extract the repository path from a Git URL
pub fn get_repo_path_from_url(url: &str) -> Result<String> {
    let parsed_url = url::Url::parse(url)
        .map_err(|e| GitError::InvalidArgument(format!("Invalid URL: {}", e)))?;
    
    // Extract the path component, removing leading and trailing slashes
    let path = parsed_url.path().trim_start_matches('/').trim_end_matches('/');
    
    // Handle special case for tor+* URLs
    if parsed_url.scheme().starts_with("tor+") {
        // For tor+* URLs, the repository might be in a path component after the host
        let path_segments: Vec<&str> = path.split('/').collect();
        if path_segments.len() > 1 {
            // Skip the first segment if it looks like a hostname
            if path_segments[0].contains('.') || path_segments[0].ends_with(".onion") {
                return Ok(path_segments[1..].join("/"));
            }
        }
    }
    
    // Handle empty path (root repository)
    if path.is_empty() {
        return Ok(String::from("."));
    }
    
    // Remove .git extension if present
    let repo_path = if path.ends_with(".git") {
        &path[0..path.len() - 4]
    } else {
        path
    };
    
    Ok(repo_path.to_string())
}