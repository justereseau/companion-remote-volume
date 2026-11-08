//! The real release keys: a manifest signed the way CI signs it (`openssl pkeyutl -sign -rawin` with
//! `UPDATE_SIGNING_KEY`) must verify against the public key built into the app. The fixture's
//! version is 0.0.1, so it can never pass as an update.
//!
//! The key was changed once, and the old private key was lost: apps built with the old public key
//! cannot verify releases signed with the current one and need a manual reinstall.
//! `update-old.json.sig` is signed with the old key, `update-old.json.new.sig` with the current one.

use remote_volume_core::update::{read_manifest, Version, PUBLIC_KEY};

const MANIFEST: &[u8] = include_bytes!("fixtures/update-old.json");
const SIGNATURE: &[u8] = include_bytes!("fixtures/update-old.json.new.sig");
const SIGNATURE_BY_PREVIOUS_KEY: &[u8] = include_bytes!("fixtures/update-old.json.sig");

/// The public key before the change: what every app installed before it carries.
const PREVIOUS_PUBLIC_KEY: [u8; 32] = [
    0x16, 0xd0, 0xd1, 0xaa, 0x40, 0x04, 0x84, 0xae, 0xa2, 0xdf, 0x89, 0xec, 0xb4, 0x56, 0xe2, 0x2c,
    0x5b, 0x51, 0x49, 0xf5, 0xbf, 0xa5, 0xf1, 0xf5, 0x38, 0x30, 0xb6, 0xf4, 0xe7, 0x58, 0xf1, 0x3d,
];

#[test]
fn a_manifest_signed_by_the_release_key_verifies_against_the_built_in_key() {
    let release = read_manifest(MANIFEST, SIGNATURE, &PUBLIC_KEY, "test-os").unwrap();
    assert_eq!(release.version, Version(0, 0, 1));
    assert_eq!(release.file, "rv-test");
}

#[test]
fn changing_one_byte_of_it_breaks_the_signature() {
    let mut changed = MANIFEST.to_vec();
    let at = changed.iter().position(|&b| b == b'0').unwrap();
    changed[at] = b'9';
    let error = read_manifest(&changed, SIGNATURE, &PUBLIC_KEY, "test-os").unwrap_err();
    assert!(error.contains("invalid signature"), "{error}");
}

#[test]
fn the_previous_key_still_verifies_what_it_signed_and_is_no_longer_accepted() {
    let release = read_manifest(
        MANIFEST,
        SIGNATURE_BY_PREVIOUS_KEY,
        &PREVIOUS_PUBLIC_KEY,
        "test-os",
    )
    .unwrap();
    assert_eq!(release.version, Version(0, 0, 1));
    let error =
        read_manifest(MANIFEST, SIGNATURE_BY_PREVIOUS_KEY, &PUBLIC_KEY, "test-os").unwrap_err();
    assert!(error.contains("invalid signature"), "{error}");
}
