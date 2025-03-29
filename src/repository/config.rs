use std::path::Path;
use std::fs;
use std::collections::HashMap;

use crate::core::{GitError, Result};

/// Represents a Git repository configuration
pub struct Config {
    sections: HashMap<String, HashMap<String, String>>,
}

impl Config {
    /// Create a new empty configuration
    pub fn new() -> Self {
        Self {
            sections: HashMap::new(),
        }
    }
    
    /// Load configuration from a file
    pub fn load_from_file(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        
        let content = fs::read_to_string(path)
            .map_err(|e| GitError::Io(e))?;
            
        Self::parse(&content)
    }
    
    /// Parse configuration content
    fn parse(content: &str) -> Result<Self> {
        let mut config = Self::new();
        let mut current_section = String::new();
        
        for line in content.lines() {
            let line = line.trim();
            
            // Skip comments and empty lines
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            
            // Parse section header: [section] or [section "subsection"]
            if line.starts_with('[') && line.ends_with(']') {
                let section_name = &line[1..line.len() - 1].trim();
                current_section = section_name.to_string();
                config.sections.entry(current_section.clone()).or_insert_with(HashMap::new);
            }
            // Parse key-value pair
            else if let Some(pos) = line.find('=') {
                let key = line[..pos].trim();
                let value = line[pos + 1..].trim();
                
                if !current_section.is_empty() {
                    if let Some(section) = config.sections.get_mut(&current_section) {
                        section.insert(key.to_string(), value.to_string());
                    }
                }
            }
        }
        
        Ok(config)
    }
    
    /// Save configuration to a file
    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        let mut content = String::new();
        
        for (section, values) in &self.sections {
            content.push_str(&format!("[{}]\n", section));
            
            for (key, value) in values {
                content.push_str(&format!("\t{} = {}\n", key, value));
            }
            
            content.push('\n');
        }
        
        fs::write(path, content).map_err(GitError::Io)?;
        Ok(())
    }
    
    /// Get a configuration value
    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections.get(section).and_then(|s| s.get(key).map(|s| s.as_str()))
    }
    
    /// Set a configuration value
    pub fn set(&mut self, section: &str, key: &str, value: &str) {
        let section_entry = self.sections.entry(section.to_string()).or_insert_with(HashMap::new);
        section_entry.insert(key.to_string(), value.to_string());
    }
}