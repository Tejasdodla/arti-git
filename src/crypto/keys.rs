use std::fmt;
use rand::rngs::OsRng;
use ed25519_dalek::{Keypair, PublicKey as DalekPublicKey, SecretKey, Signature, Signer as DalekSigner, Verifier as DalekVerifier};
use base64::{Engine as _, engine::general_purpose};

use super::signing::{Signer, Verifier, SignatureError};

/// Ed25519 public key
#[derive(Clone)]
pub struct PublicKey(DalekPublicKey);

impl PublicKey {
    /// Create a new public key from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SignatureError> {
        let key = DalekPublicKey::from_bytes(bytes)
            .map_err(|e| SignatureError::InvalidKeyFormat(format!("Invalid public key: {}", e)))?;
        Ok(Self(key))
    }
    
    /// Get the raw bytes of the public key
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
    
    /// Convert the public key to a Base64-encoded string
    pub fn to_base64(&self) -> String {
        general_purpose::STANDARD.encode(self.0.as_bytes())
    }
    
    /// Create a public key from a Base64-encoded string
    pub fn from_base64(encoded: &str) -> Result<Self, SignatureError> {
        let bytes = general_purpose::STANDARD.decode(encoded)
            .map_err(|e| SignatureError::InvalidKeyFormat(format!("Invalid Base64 encoding: {}", e)))?;
            
        Self::from_bytes(&bytes)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.to_base64())
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_base64())
    }
}

impl Verifier for PublicKey {
    fn verify(&self, data: &[u8], signature: &[u8]) -> Result<bool, SignatureError> {
        let sig = Signature::from_bytes(signature)
            .map_err(|e| SignatureError::InvalidKeyFormat(format!("Invalid signature format: {}", e)))?;
            
        self.0.verify(data, &sig)
            .map(|_| true)
            .or_else(|e| {
                if e == ed25519_dalek::SignatureError::SignatureMismatch {
                    Ok(false)
                } else {
                    Err(SignatureError::VerificationError(format!("Verification failed: {}", e)))
                }
            })
    }
}

/// Ed25519 private key
pub struct PrivateKey(SecretKey);

impl PrivateKey {
    /// Create a new private key from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SignatureError> {
        let key = SecretKey::from_bytes(bytes)
            .map_err(|e| SignatureError::InvalidKeyFormat(format!("Invalid private key: {}", e)))?;
        Ok(Self(key))
    }
    
    /// Get the raw bytes of the private key
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

/// Ed25519 keypair for signing and verification
pub struct KeyPair {
    keypair: Keypair,
}

impl KeyPair {
    /// Generate a new random keypair
    pub fn generate() -> Self {
        let mut csprng = OsRng;
        let keypair = Keypair::generate(&mut csprng);
        Self { keypair }
    }
    
    /// Create a keypair from separate public and private keys
    pub fn from_keys(public: &PublicKey, private: &PrivateKey) -> Result<Self, SignatureError> {
        let keypair = Keypair {
            public: public.0,
            secret: private.0.clone(),
        };
        
        Ok(Self { keypair })
    }
    
    /// Get the public key from this keypair
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.keypair.public)
    }
    
    /// Create a keypair from a seed
    pub fn from_seed(seed: &[u8]) -> Result<Self, SignatureError> {
        if seed.len() != 32 {
            return Err(SignatureError::InvalidKeyFormat("Seed must be 32 bytes".to_string()));
        }
        
        let mut seed_array = [0u8; 32];
        seed_array.copy_from_slice(seed);
        
        let keypair = Keypair::from_seed(&seed_array)
            .map_err(|e| SignatureError::InvalidKeyFormat(format!("Invalid seed: {}", e)))?;
            
        Ok(Self { keypair })
    }
}

impl Signer for KeyPair {
    fn sign(&self, data: &[u8]) -> Result<Vec<u8>, SignatureError> {
        let signature = self.keypair.sign(data);
        Ok(signature.to_bytes().to_vec())
    }
    
    fn public_key(&self) -> Vec<u8> {
        self.keypair.public.as_bytes().to_vec()
    }
}

impl Verifier for KeyPair {
    fn verify(&self, data: &[u8], signature: &[u8]) -> Result<bool, SignatureError> {
        let public_key = PublicKey(self.keypair.public);
        public_key.verify(data, signature)
    }
}