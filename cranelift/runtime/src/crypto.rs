use crate::bytes::{pith_bytes_from_vec, pith_bytes_ref};
use crate::handle_registry::{self, HandleKind};
use ring::{aead, agreement, rand, signature};
use std::fs;

struct PithX25519Key {
    key: Option<agreement::EphemeralPrivateKey>,
    public_key: Vec<u8>,
}

unsafe fn bytes_slice<'a>(handle: i64) -> Option<&'a [u8]> {
    Some(pith_bytes_ref(handle)?.data.as_slice())
}

unsafe fn x25519_key_ref<'a>(handle: i64) -> Option<&'a PithX25519Key> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::X25519Key) {
        return None;
    }
    Some(&*(handle as *const PithX25519Key))
}

unsafe fn x25519_key_mut<'a>(handle: i64) -> Option<&'a mut PithX25519Key> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::X25519Key) {
        return None;
    }
    Some(&mut *(handle as *mut PithX25519Key))
}

/// an AEAD key and nonce from raw bytes, or `None` for a key of the wrong
/// length or a nonce that is not 12 bytes.
fn aead_key_nonce(
    alg: &'static aead::Algorithm,
    key: &[u8],
    nonce: &[u8],
) -> Option<(aead::LessSafeKey, aead::Nonce)> {
    if nonce.len() != 12 {
        return None;
    }
    let unbound = aead::UnboundKey::new(alg, key).ok()?;
    let nonce = aead::Nonce::try_assume_unique_for_key(nonce).ok()?;
    Some((aead::LessSafeKey::new(unbound), nonce))
}

/// encrypt `plaintext` and append the tag. every argument is a bytes handle;
/// the result is a bytes handle, or 0 when any input is invalid.
unsafe fn seal(alg: &'static aead::Algorithm, key: i64, nonce: i64, aad: i64, plaintext: i64) -> i64 {
    let (Some(key), Some(nonce), Some(aad), Some(plaintext)) =
        (bytes_slice(key), bytes_slice(nonce), bytes_slice(aad), bytes_slice(plaintext))
    else {
        return 0;
    };
    let Some((key, nonce)) = aead_key_nonce(alg, key, nonce) else {
        return 0;
    };
    let mut out = plaintext.to_vec();
    if key
        .seal_in_place_append_tag(nonce, aead::Aad::from(aad), &mut out)
        .is_err()
    {
        return 0;
    }
    pith_bytes_from_vec(out)
}

/// check the tag and decrypt `ciphertext`. every argument is a bytes handle;
/// the result is a bytes handle, or 0 when any input is invalid or the tag
/// does not match.
unsafe fn open(alg: &'static aead::Algorithm, key: i64, nonce: i64, aad: i64, ciphertext: i64) -> i64 {
    let (Some(key), Some(nonce), Some(aad), Some(ciphertext)) =
        (bytes_slice(key), bytes_slice(nonce), bytes_slice(aad), bytes_slice(ciphertext))
    else {
        return 0;
    };
    let Some((key, nonce)) = aead_key_nonce(alg, key, nonce) else {
        return 0;
    };
    let mut in_out = ciphertext.to_vec();
    let Ok(plain) = key.open_in_place(nonce, aead::Aad::from(aad), &mut in_out) else {
        return 0;
    };
    pith_bytes_from_vec(plain.to_vec())
}

/// 1 when `sig` is a valid signature of `message` under `public_key`, else 0.
/// every argument is a bytes handle; an invalid one verifies nothing.
unsafe fn verify(
    alg: &'static dyn signature::VerificationAlgorithm,
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    let (Some(public_key), Some(message), Some(sig)) =
        (bytes_slice(public_key), bytes_slice(message), bytes_slice(sig))
    else {
        return 0;
    };
    let key = signature::UnparsedPublicKey::new(alg, public_key);
    if key.verify(message, sig).is_ok() {
        1
    } else {
        0
    }
}

/// sign `message` with an RSA key in pkcs#8 form. both arguments are bytes
/// handles; the result is a bytes handle, or 0 on an invalid input.
unsafe fn sign_rsa(encoding: &'static dyn signature::RsaEncoding, pkcs8: i64, message: i64) -> i64 {
    let (Some(pkcs8), Some(message)) = (bytes_slice(pkcs8), bytes_slice(message)) else {
        return 0;
    };
    let Ok(key_pair) = signature::RsaKeyPair::from_pkcs8(pkcs8) else {
        return 0;
    };
    let rng = rand::SystemRandom::new();
    let mut sig = vec![0_u8; key_pair.public().modulus_len()];
    if key_pair.sign(encoding, &rng, message, &mut sig).is_err() {
        return 0;
    }
    pith_bytes_from_vec(sig)
}

/// sign `message` with an ECDSA key in pkcs#8 form. both arguments are bytes
/// handles; the result is a bytes handle, or 0 on an invalid input. the
/// per-signature nonce comes from the system rng inside ring and is never
/// chosen here, which is what keeps ecdsa keys from leaking through a
/// repeated nonce.
unsafe fn sign_ecdsa(alg: &'static signature::EcdsaSigningAlgorithm, pkcs8: i64, message: i64) -> i64 {
    let (Some(pkcs8), Some(message)) = (bytes_slice(pkcs8), bytes_slice(message)) else {
        return 0;
    };
    let rng = rand::SystemRandom::new();
    let Ok(key_pair) = signature::EcdsaKeyPair::from_pkcs8(alg, pkcs8, &rng) else {
        return 0;
    };
    let Ok(sig) = key_pair.sign(&rng, message) else {
        return 0;
    };
    pith_bytes_from_vec(sig.as_ref().to_vec())
}

fn ecdh_keygen(alg: &'static agreement::Algorithm) -> i64 {
    let rng = rand::SystemRandom::new();
    let Ok(key) = agreement::EphemeralPrivateKey::generate(alg, &rng) else {
        return 0;
    };
    let Ok(public_key) = key.compute_public_key() else {
        return 0;
    };
    let ptr = Box::into_raw(Box::new(PithX25519Key {
        key: Some(key),
        public_key: public_key.as_ref().to_vec(),
    }));
    handle_registry::register(ptr as *const (), HandleKind::X25519Key);
    ptr as i64
}

#[no_mangle]
pub extern "C" fn pith_crypto_x25519_keygen() -> i64 {
    ecdh_keygen(&agreement::X25519)
}

// a p-256 key rides the same handle plumbing as x25519: the private key
// remembers its own algorithm, so public_key/shared_secret/close serve both.
#[no_mangle]
pub extern "C" fn pith_crypto_p256_keygen() -> i64 {
    ecdh_keygen(&agreement::ECDH_P256)
}

/// Generate a P-384 (secp384r1) ephemeral key pair for ECDH.
#[no_mangle]
pub extern "C" fn pith_crypto_p384_keygen() -> i64 {
    ecdh_keygen(&agreement::ECDH_P384)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_x25519_public_key(handle: i64) -> i64 {
    let Some(key) = x25519_key_ref(handle) else {
        return 0;
    };
    pith_bytes_from_vec(key.public_key.clone())
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_x25519_shared_secret(
    handle: i64,
    peer_public_key: i64,
) -> i64 {
    let Some(peer) = bytes_slice(peer_public_key) else {
        return 0;
    };
    let Some(key) = x25519_key_mut(handle) else {
        return 0;
    };
    let Some(private_key) = key.key.take() else {
        return 0;
    };
    let peer_key = agreement::UnparsedPublicKey::new(private_key.algorithm(), peer);
    agreement::agree_ephemeral(private_key, &peer_key, |secret| {
        pith_bytes_from_vec(secret.to_vec())
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn pith_crypto_x25519_close(handle: i64) {
    if !handle_registry::is_valid(handle as *const (), HandleKind::X25519Key) {
        return;
    }
    handle_registry::unregister(handle as *const (), HandleKind::X25519Key);
    let _ = unsafe { Box::from_raw(handle as *mut PithX25519Key) };
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_aes_256_gcm_seal(
    key: i64,
    nonce: i64,
    aad: i64,
    plaintext: i64,
) -> i64 {
    seal(&aead::AES_256_GCM, key, nonce, aad, plaintext)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_aes_256_gcm_open(
    key: i64,
    nonce: i64,
    aad: i64,
    ciphertext: i64,
) -> i64 {
    open(&aead::AES_256_GCM, key, nonce, aad, ciphertext)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_aes_128_gcm_seal(
    key: i64,
    nonce: i64,
    aad: i64,
    plaintext: i64,
) -> i64 {
    seal(&aead::AES_128_GCM, key, nonce, aad, plaintext)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_aes_128_gcm_open(
    key: i64,
    nonce: i64,
    aad: i64,
    ciphertext: i64,
) -> i64 {
    open(&aead::AES_128_GCM, key, nonce, aad, ciphertext)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_chacha20_poly1305_seal(
    key: i64,
    nonce: i64,
    aad: i64,
    plaintext: i64,
) -> i64 {
    seal(&aead::CHACHA20_POLY1305, key, nonce, aad, plaintext)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_chacha20_poly1305_open(
    key: i64,
    nonce: i64,
    aad: i64,
    ciphertext: i64,
) -> i64 {
    open(&aead::CHACHA20_POLY1305, key, nonce, aad, ciphertext)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_ed25519(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    verify(&signature::ED25519, public_key, message, sig)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_ecdsa_p256_sha256_asn1(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    verify(&signature::ECDSA_P256_SHA256_ASN1, public_key, message, sig)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_ecdsa_p256_sha384_asn1(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    verify(&signature::ECDSA_P256_SHA384_ASN1, public_key, message, sig)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_ecdsa_p384_sha384_asn1(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    verify(&signature::ECDSA_P384_SHA384_ASN1, public_key, message, sig)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_rsa_pkcs1_sha256(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    verify(&signature::RSA_PKCS1_2048_8192_SHA256, public_key, message, sig)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_rsa_pkcs1_sha384(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    verify(&signature::RSA_PKCS1_2048_8192_SHA384, public_key, message, sig)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_rsa_pss_sha256(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    verify(&signature::RSA_PSS_2048_8192_SHA256, public_key, message, sig)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_rsa_pss_sha384(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    verify(&signature::RSA_PSS_2048_8192_SHA384, public_key, message, sig)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_rsa_pss_sha512(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    verify(&signature::RSA_PSS_2048_8192_SHA512, public_key, message, sig)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_rsa_pkcs1_sha512(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    verify(&signature::RSA_PKCS1_2048_8192_SHA512, public_key, message, sig)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_sign_rsa_pss_sha256_pkcs8(pkcs8: i64, message: i64) -> i64 {
    sign_rsa(&signature::RSA_PSS_SHA256, pkcs8, message)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_sign_rsa_pss_sha384_pkcs8(pkcs8: i64, message: i64) -> i64 {
    sign_rsa(&signature::RSA_PSS_SHA384, pkcs8, message)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_sign_rsa_pss_sha512_pkcs8(pkcs8: i64, message: i64) -> i64 {
    sign_rsa(&signature::RSA_PSS_SHA512, pkcs8, message)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_sign_rsa_pkcs1_sha384_pkcs8(pkcs8: i64, message: i64) -> i64 {
    sign_rsa(&signature::RSA_PKCS1_SHA384, pkcs8, message)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_sign_rsa_pkcs1_sha512_pkcs8(pkcs8: i64, message: i64) -> i64 {
    sign_rsa(&signature::RSA_PKCS1_SHA512, pkcs8, message)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_sign_ecdsa_p384_sha384_asn1_pkcs8(pkcs8: i64, message: i64) -> i64 {
    sign_ecdsa(&signature::ECDSA_P384_SHA384_ASN1_SIGNING, pkcs8, message)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_sign_rsa_pkcs1_sha256_pkcs8(pkcs8: i64, message: i64) -> i64 {
    sign_rsa(&signature::RSA_PKCS1_SHA256, pkcs8, message)
}

/// Sign `message` with an ed25519 private key in pkcs#8 form. The
/// maybe_unchecked parser is what accepts openssl's output: openssl writes
/// pkcs#8 v1 (seed only), and ring's strict parser demands v2 with the
/// public key embedded. "unchecked" only skips the seed/public-key
/// consistency check that v1 makes impossible. The signature itself is
/// deterministic, so there is no rng to mishandle. Returns a 64-byte
/// signature handle, or 0 on a malformed key.
#[no_mangle]
pub unsafe extern "C" fn pith_crypto_sign_ed25519_pkcs8(pkcs8: i64, message: i64) -> i64 {
    let (Some(pkcs8), Some(message)) = (bytes_slice(pkcs8), bytes_slice(message)) else {
        return 0;
    };
    let Ok(key_pair) = signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(pkcs8) else {
        return 0;
    };
    pith_bytes_from_vec(key_pair.sign(message).as_ref().to_vec())
}

/// Sign `message` with a p-256 private key in pkcs#8 form. Uses the FIXED
/// encoding, so the signature comes out as raw r‖s (64 bytes), the form jws
/// carries, rather than asn.1 der. Returns 0 on a malformed key.
#[no_mangle]
pub unsafe extern "C" fn pith_crypto_sign_ecdsa_p256_sha256_pkcs8(pkcs8: i64, message: i64) -> i64 {
    sign_ecdsa(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8, message)
}

/// Sign `message` with an ECDSA P-256 key (SHA-256), producing an ASN.1 DER
/// signature, the form TLS carries in ServerKeyExchange and CertificateVerify,
/// unlike the raw r||s of the FIXED variant above. Returns 0 on a malformed
/// key.
#[no_mangle]
pub unsafe extern "C" fn pith_crypto_sign_ecdsa_p256_sha256_asn1_pkcs8(pkcs8: i64, message: i64) -> i64 {
    sign_ecdsa(&signature::ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8, message)
}

/// BLAKE2b digest of `data`, keyed when `key` is non-empty. `out_len` selects
/// the digest length (1 to 64 bytes). Returns a bytes handle, or 0 on invalid
/// input.
#[no_mangle]
pub unsafe extern "C" fn pith_crypto_blake2b(data: i64, key: i64, out_len: i64) -> i64 {
    let Some(data) = bytes_slice(data) else {
        return 0;
    };
    let Some(key) = bytes_slice(key) else {
        return 0;
    };
    if out_len < 1 || out_len > crate::blake2b::MAX_OUT_LEN as i64 {
        return 0;
    }
    match crate::blake2b::hash(out_len as usize, key, data) {
        Some(digest) => pith_bytes_from_vec(digest),
        None => 0,
    }
}

/// Argon2id tag for `password` and `salt` with the given time cost (passes),
/// memory cost (KiB), and parallelism. Returns a bytes handle, or 0 when a
/// parameter is out of range (including the runtime's memory-cost cap).
#[no_mangle]
pub unsafe extern "C" fn pith_crypto_argon2id(
    password: i64,
    salt: i64,
    passes: i64,
    memory_kib: i64,
    lanes: i64,
    out_len: i64,
) -> i64 {
    let Some(password) = bytes_slice(password) else {
        return 0;
    };
    let Some(salt) = bytes_slice(salt) else {
        return 0;
    };
    // the argon2 module re-validates ranges; these checks only make the
    // i64 -> u32/usize conversions safe
    if passes < 0 || memory_kib < 0 || lanes < 0 || out_len < 0 {
        return 0;
    }
    if passes > u32::MAX as i64 || memory_kib > u32::MAX as i64 || lanes > u32::MAX as i64 {
        return 0;
    }
    match crate::argon2::argon2id(
        password,
        salt,
        b"",
        b"",
        passes as u32,
        memory_kib as u32,
        lanes as u32,
        out_len as usize,
    ) {
        Ok(tag) => pith_bytes_from_vec(tag),
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn pith_os_cert_roots_pem() -> *mut i8 {
    let mut candidates = Vec::new();
    // through the overlay, so a program that sets SSL_CERT_FILE from pith is
    // still heard: `set_env` no longer writes to libc (see `env_overlay`).
    if let Some(path) = crate::env_overlay::get("SSL_CERT_FILE") {
        candidates.push(path);
    }
    candidates.extend([
        "/etc/ssl/certs/ca-certificates.crt".to_string(),
        "/etc/pki/tls/certs/ca-bundle.crt".to_string(),
        "/etc/ssl/ca-bundle.pem".to_string(),
        "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem".to_string(),
    ]);

    for path in candidates {
        if let Ok(data) = fs::read(&path) {
            if data
                .windows(27)
                .any(|window| window == b"-----BEGIN CERTIFICATE-----")
            {
                return unsafe { crate::pith_copy_bytes_to_cstring(&data) };
            }
        }
    }
    unsafe { crate::pith_cstring_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_x25519_handles_return_safe_defaults() {
        unsafe {
            assert_eq!(pith_crypto_x25519_public_key(12345), 0);
            assert_eq!(pith_crypto_x25519_shared_secret(12345, 0), 0);
        }
        pith_crypto_x25519_close(12345);
    }

    #[test]
    fn closed_x25519_handle_is_rejected() {
        let handle = pith_crypto_x25519_keygen();
        assert!(handle > 0);
        pith_crypto_x25519_close(handle);
        unsafe {
            assert_eq!(pith_crypto_x25519_public_key(handle), 0);
            assert_eq!(pith_crypto_x25519_shared_secret(handle, 0), 0);
        }
        pith_crypto_x25519_close(handle);
    }
}
