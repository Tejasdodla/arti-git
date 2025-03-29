use std::fmt;
use crate::crypto::{KeyPair, PublicKey, SignatureError, Signer, Verifier};

/// Generic identity trait for Git operations
pub trait Identity: Signer + Verifier {
    /// Get the identity's name
    fn name(&self) -> &str;
    
    /// Get the identity's email
    fn email(&self) -> &str;
    
    /// Get a formatted string for Git commits
    fn formatted(&self) -> String {
        format!("{} <{}>", self.name(), self.email())
    }
    
    /// Get the identity's public key fingerprint
    fn fingerprint(&self) -> String;
}

/// An anonymous identity for Git operations over Tor
/// 
/// Uses the key's fingerprint as the name and an .onion address as the email
pub struct AnonymousIdentity {
    name: String,
    email: String,
    key_pair: KeyPair,
}

impl AnonymousIdentity {
    /// Create a new anonymous identity from a key pair and onion address
    pub fn new(key_pair: KeyPair, onion_address: &str) -> Self {
        // Use the first 8 characters of the public key's fingerprint as the name
        let public_key = key_pair.public_key();
        let fingerprint = Self::calculate_fingerprint(&public_key);
        let name = fingerprint.chars().take(8).collect::<String>();
        
        // Use the onion address as the email
        let email = format!("{}@anonymous.onion", onion_address.trim_end_matches(".onion"));
        
        Self { name, email, key_pair }
    }
    
    /// Generate a new random anonymous identity
    pub fn generate(onion_address: &str) -> Self {
        let key_pair = KeyPair::generate();
        Self::new(key_pair, onion_address)
    }
    
    /// Calculate a fingerprint (simplified format) from a public key
    fn calculate_fingerprint(public_key: &PublicKey) -> String {
        let bytes = public_key.as_bytes();
        // Use a hex representation of the key as the fingerprint
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

impl fmt::Display for AnonymousIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} <{}>", self.name, self.email)
    }
}

impl Identity for AnonymousIdentity {
    fn name(&self) -> &str {
        &self.name
    }
    
    fn email(&self) -> &str {
        &self.email
    }
    
    fn fingerprint(&self) -> String {
        Self::calculate_fingerprint(&self.key_pair.public_key())
    }
}

impl Signer for AnonymousIdentity {
    fn sign(&self, data: &[u8]) -> Result<Vec<u8>, SignatureError> {
        self.key_pair.sign(data)
    }
    
    fn public_key(&self) -> Vec<u8> {
        self.key_pair.public_key().as_bytes().to_vec()
    }
}

impl Verifier for AnonymousIdentity {
    fn verify(&self, data: &[u8], signature: &[u8]) -> Result<bool, SignatureError> {
        self.key_pair.verify(data, signature)
    }
}