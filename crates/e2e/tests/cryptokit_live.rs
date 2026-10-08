#![cfg(target_os = "macos")]

#[path = "../../test_support.rs"]
mod test_support;

use alondra_e2e::http_challenge_bytes;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use std::{path::PathBuf, process::Command};

#[test]
fn freshly_generated_cryptokit_signature_verifies_in_rust() {
    let scratch_directory = crate::test_support::Directory::new();
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cryptokit_vector.swift");
    let cache = scratch_directory.parent().unwrap().join(format!("alondra-cryptokit-test-{}", std::process::id()));
    let output = Command::new("swift")
        .arg(script)
        .env("SWIFT_MODULE_CACHE_PATH", cache.join("swift"))
        .env("CLANG_MODULE_CACHE_PATH", cache.join("clang"))
        .output()
        .expect("run CryptoKit vector");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("CryptoKit verified HKDF key"));
    let encoded = stdout
        .lines()
        .find_map(|line| line.strip_prefix("swift_http_signature="))
        .expect("Swift signature output");
    let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(encoded).unwrap()).unwrap();
    let signer = p256::ecdsa::SigningKey::from_slice(&[1u8; 32]).unwrap();
    let challenge = http_challenge_bytes(
        "mac-1",
        "POST",
        "/v1/work?after=2",
        &[7u8; 16],
        b"payload",
        1_700_000_000_000,
        &[9u8; 16],
    )
    .unwrap();
    VerifyingKey::from(signer)
        .verify(&challenge, &signature)
        .unwrap();
}
