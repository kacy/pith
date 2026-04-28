use crate::bytes::{pith_bytes_from_vec, pith_bytes_ref};
use ring::{aead, agreement, rand, signature};
use std::fs;

struct PithX25519Key {
    key: Option<agreement::EphemeralPrivateKey>,
    public_key: Vec<u8>,
}

unsafe fn bytes_slice<'a>(handle: i64) -> Option<&'a [u8]> {
    Some(pith_bytes_ref(handle)?.data.as_slice())
}

fn seal_with(
    alg: &'static aead::Algorithm,
    key: &[u8],
    nonce: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> i64 {
    if nonce.len() != 12 {
        return 0;
    }
    let Ok(unbound) = aead::UnboundKey::new(alg, key) else {
        return 0;
    };
    let Ok(nonce) = aead::Nonce::try_assume_unique_for_key(nonce) else {
        return 0;
    };
    let key = aead::LessSafeKey::new(unbound);
    let mut out = plaintext.to_vec();
    if key
        .seal_in_place_append_tag(nonce, aead::Aad::from(aad), &mut out)
        .is_err()
    {
        return 0;
    }
    pith_bytes_from_vec(out)
}

fn open_with(
    alg: &'static aead::Algorithm,
    key: &[u8],
    nonce: &[u8],
    aad: &[u8],
    ciphertext: &[u8],
) -> i64 {
    if nonce.len() != 12 {
        return 0;
    }
    let Ok(unbound) = aead::UnboundKey::new(alg, key) else {
        return 0;
    };
    let Ok(nonce) = aead::Nonce::try_assume_unique_for_key(nonce) else {
        return 0;
    };
    let key = aead::LessSafeKey::new(unbound);
    let mut in_out = ciphertext.to_vec();
    let Ok(plain) = key.open_in_place(nonce, aead::Aad::from(aad), &mut in_out) else {
        return 0;
    };
    pith_bytes_from_vec(plain.to_vec())
}

fn verify_with(
    alg: &'static dyn signature::VerificationAlgorithm,
    public_key: &[u8],
    message: &[u8],
    sig: &[u8],
) -> i64 {
    let key = signature::UnparsedPublicKey::new(alg, public_key);
    if key.verify(message, sig).is_ok() {
        1
    } else {
        0
    }
}

fn sign_rsa_with(
    encoding: &'static dyn signature::RsaEncoding,
    pkcs8: &[u8],
    message: &[u8],
) -> i64 {
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

#[no_mangle]
pub extern "C" fn pith_crypto_x25519_keygen() -> i64 {
    let rng = rand::SystemRandom::new();
    let Ok(key) = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rng) else {
        return 0;
    };
    let Ok(public_key) = key.compute_public_key() else {
        return 0;
    };
    Box::into_raw(Box::new(PithX25519Key {
        key: Some(key),
        public_key: public_key.as_ref().to_vec(),
    })) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_x25519_public_key(handle: i64) -> i64 {
    if handle <= 0 {
        return 0;
    }
    let key = &*(handle as *const PithX25519Key);
    pith_bytes_from_vec(key.public_key.clone())
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_x25519_shared_secret(
    handle: i64,
    peer_public_key: i64,
) -> i64 {
    if handle <= 0 {
        return 0;
    }
    let Some(peer) = bytes_slice(peer_public_key) else {
        return 0;
    };
    let key = &mut *(handle as *mut PithX25519Key);
    let Some(private_key) = key.key.take() else {
        return 0;
    };
    let peer_key = agreement::UnparsedPublicKey::new(&agreement::X25519, peer);
    agreement::agree_ephemeral(private_key, &peer_key, |secret| {
        pith_bytes_from_vec(secret.to_vec())
    })
    .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn pith_crypto_x25519_close(handle: i64) {
    if handle <= 0 {
        return;
    }
    let _ = unsafe { Box::from_raw(handle as *mut PithX25519Key) };
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_aes_128_gcm_seal(
    key: i64,
    nonce: i64,
    aad: i64,
    plaintext: i64,
) -> i64 {
    let Some(key) = bytes_slice(key) else {
        return 0;
    };
    let Some(nonce) = bytes_slice(nonce) else {
        return 0;
    };
    let Some(aad) = bytes_slice(aad) else {
        return 0;
    };
    let Some(plaintext) = bytes_slice(plaintext) else {
        return 0;
    };
    seal_with(&aead::AES_128_GCM, key, nonce, aad, plaintext)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_aes_128_gcm_open(
    key: i64,
    nonce: i64,
    aad: i64,
    ciphertext: i64,
) -> i64 {
    let Some(key) = bytes_slice(key) else {
        return 0;
    };
    let Some(nonce) = bytes_slice(nonce) else {
        return 0;
    };
    let Some(aad) = bytes_slice(aad) else {
        return 0;
    };
    let Some(ciphertext) = bytes_slice(ciphertext) else {
        return 0;
    };
    open_with(&aead::AES_128_GCM, key, nonce, aad, ciphertext)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_chacha20_poly1305_seal(
    key: i64,
    nonce: i64,
    aad: i64,
    plaintext: i64,
) -> i64 {
    let Some(key) = bytes_slice(key) else {
        return 0;
    };
    let Some(nonce) = bytes_slice(nonce) else {
        return 0;
    };
    let Some(aad) = bytes_slice(aad) else {
        return 0;
    };
    let Some(plaintext) = bytes_slice(plaintext) else {
        return 0;
    };
    seal_with(&aead::CHACHA20_POLY1305, key, nonce, aad, plaintext)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_chacha20_poly1305_open(
    key: i64,
    nonce: i64,
    aad: i64,
    ciphertext: i64,
) -> i64 {
    let Some(key) = bytes_slice(key) else {
        return 0;
    };
    let Some(nonce) = bytes_slice(nonce) else {
        return 0;
    };
    let Some(aad) = bytes_slice(aad) else {
        return 0;
    };
    let Some(ciphertext) = bytes_slice(ciphertext) else {
        return 0;
    };
    open_with(&aead::CHACHA20_POLY1305, key, nonce, aad, ciphertext)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_ed25519(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    let Some(public_key) = bytes_slice(public_key) else {
        return 0;
    };
    let Some(message) = bytes_slice(message) else {
        return 0;
    };
    let Some(sig) = bytes_slice(sig) else {
        return 0;
    };
    verify_with(&signature::ED25519, public_key, message, sig)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_ecdsa_p256_sha256_asn1(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    let Some(public_key) = bytes_slice(public_key) else {
        return 0;
    };
    let Some(message) = bytes_slice(message) else {
        return 0;
    };
    let Some(sig) = bytes_slice(sig) else {
        return 0;
    };
    verify_with(&signature::ECDSA_P256_SHA256_ASN1, public_key, message, sig)
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_rsa_pkcs1_sha256(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    let Some(public_key) = bytes_slice(public_key) else {
        return 0;
    };
    let Some(message) = bytes_slice(message) else {
        return 0;
    };
    let Some(sig) = bytes_slice(sig) else {
        return 0;
    };
    verify_with(
        &signature::RSA_PKCS1_2048_8192_SHA256,
        public_key,
        message,
        sig,
    )
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_verify_rsa_pss_sha256(
    public_key: i64,
    message: i64,
    sig: i64,
) -> i64 {
    let Some(public_key) = bytes_slice(public_key) else {
        return 0;
    };
    let Some(message) = bytes_slice(message) else {
        return 0;
    };
    let Some(sig) = bytes_slice(sig) else {
        return 0;
    };
    verify_with(
        &signature::RSA_PSS_2048_8192_SHA256,
        public_key,
        message,
        sig,
    )
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_sign_rsa_pss_sha256_pkcs8(pkcs8: i64, message: i64) -> i64 {
    let Some(pkcs8) = bytes_slice(pkcs8) else {
        return 0;
    };
    let Some(message) = bytes_slice(message) else {
        return 0;
    };
    sign_rsa_with(&signature::RSA_PSS_SHA256, pkcs8, message)
}

#[no_mangle]
pub extern "C" fn pith_os_cert_roots_pem() -> *mut i8 {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("SSL_CERT_FILE") {
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
