use std::fmt;
use ed25519_dalek::{Signature, SignatureError as DalekSignatureError, Signer as DalekSigner, Verifier as DalekVerifier};

/// Error type for signature operations
#[derive(Debug)]
pub enum SignatureError {
    /// Error during signing process
    SigningError(String),
    
    /// Error during verification process
    VerificationError(String),
    
    /// Invalid key format
    InvalidKeyFormat(String),
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignatureError::SigningError(msg) => write!(f, "Signing error: {}", msg),
            SignatureError::VerificationError(msg) => write!(f, "Verification error: {}", msg),
            SignatureError::InvalidKeyFormat(msg) => write!(f, "Invalid key format: {}", msg),
        }
    }
}

impl std::error::Error for SignatureError {}

impl From<DalekSignatureError> for SignatureError {
    fn from(err: DalekSignatureError) -> Self {
        SignatureError::VerificationError(format!("{}", err))
    }
}

/// Generic trait for signing data
pub trait Signer {
    /// Sign the given data
    fn sign(&self, data: &[u8]) -> Result<Vec<u8>, SignatureError>;
    
    /// Get the signer's public key
    fn public_key(&self) -> Vec<u8>;
}

/// Generic trait for verifying signatures
pub trait Verifier {
    /// Verify the signature on the given data
    fn verify(&self, data: &[u8], signature: &[u8]) -> Result<bool, SignatureError>;
}