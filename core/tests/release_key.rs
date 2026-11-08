//! The real release key: a manifest signed the way CI signs it (`openssl pkeyutl -sign -rawin` with
//! `UPDATE_SIGNING_KEY`) must verify against the public key built into the app. The fixture's
//! version is 0.0.1, so it can never pass as an update.

use remote_volume_core::update::{read_manifest, Version, PUBLIC_KEY};

const MANIFEST: &[u8] = include_bytes!("fixtures/update-old.json");
const SIGNATURE: &[u8] = include_bytes!("fixtures/update-old.json.sig");

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
