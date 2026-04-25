use blake3::Hasher;
use md5::{Digest, Md5};
use ring::{
    digest, hmac,
    rand::{SecureRandom, SystemRandom},
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

pub enum CryptoContext {
    Hash(digest::Context),
    Hmac(hmac::Context),
    Md5(Md5),
    Blake3(Hasher),
}

thread_local! {
    static CRYPTO_REGISTRY: RefCell<HashMap<u64, CryptoContext>> = RefCell::new(HashMap::new());
}

static NEXT_CONTEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn random_bytes(size: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; size];
    let rng = SystemRandom::new();
    rng.fill(&mut buf)
        .map_err(|_| "Failed to generate random bytes".to_string())?;
    Ok(buf)
}

pub fn create_hash(algorithm: &str) -> Result<u64, String> {
    let id = NEXT_CONTEXT_ID.fetch_add(1, Ordering::SeqCst);
    let ctx = match algorithm.to_lowercase().as_str() {
        "sha256" => CryptoContext::Hash(digest::Context::new(&digest::SHA256)),
        "sha512" => CryptoContext::Hash(digest::Context::new(&digest::SHA512)),
        "sha384" => CryptoContext::Hash(digest::Context::new(&digest::SHA384)),
        "sha1" => CryptoContext::Hash(digest::Context::new(&digest::SHA1_FOR_LEGACY_USE_ONLY)),
        "md5" => CryptoContext::Md5(Md5::new()),
        "blake3" => CryptoContext::Blake3(Hasher::new()),
        _ => {
            return Err(format!("Unsupported hash algorithm: {}", algorithm));
        }
    };
    CRYPTO_REGISTRY.with(|r| {
        r.borrow_mut().insert(id, ctx);
    });
    Ok(id)
}

pub fn create_hmac(algorithm: &str, key_bytes: &[u8]) -> Result<u64, String> {
    let alg = match algorithm.to_lowercase().as_str() {
        "sha256" => hmac::HMAC_SHA256,
        "sha512" => hmac::HMAC_SHA512,
        "sha384" => hmac::HMAC_SHA384,
        _ => return Err(format!("Unsupported HMAC algorithm: {}", algorithm)),
    };
    let key = hmac::Key::new(alg, key_bytes);
    let context = hmac::Context::with_key(&key);
    let id = NEXT_CONTEXT_ID.fetch_add(1, Ordering::SeqCst);
    CRYPTO_REGISTRY.with(|r| {
        r.borrow_mut().insert(id, CryptoContext::Hmac(context));
    });
    Ok(id)
}

pub fn update(id: u64, data: &[u8]) -> Result<(), String> {
    CRYPTO_REGISTRY.with(|r| {
        let mut registry = r.borrow_mut();
        if let Some(ctx) = registry.get_mut(&id) {
            match ctx {
                CryptoContext::Hash(c) => c.update(data),
                CryptoContext::Hmac(c) => c.update(data),
                CryptoContext::Md5(h) => {
                    h.update(data);
                }
                CryptoContext::Blake3(h) => {
                    h.update(data);
                }
            }
            Ok(())
        } else {
            Err("Crypto context not found".to_string())
        }
    })
}

pub fn digest(id: u64) -> Result<Vec<u8>, String> {
    CRYPTO_REGISTRY.with(|r| {
        let mut registry = r.borrow_mut();
        if let Some(ctx) = registry.remove(&id) {
            match ctx {
                CryptoContext::Hash(c) => Ok(c.finish().as_ref().to_vec()),
                CryptoContext::Hmac(c) => Ok(c.sign().as_ref().to_vec()),
                CryptoContext::Md5(h) => Ok(h.finalize().to_vec()),
                CryptoContext::Blake3(h) => Ok(h.finalize().as_bytes().to_vec()),
            }
        } else {
            Err("Crypto context not found".to_string())
        }
    })
}

pub(crate) fn clear_thread_local_registry() {
    CRYPTO_REGISTRY.with(|r| r.borrow_mut().clear());
}
