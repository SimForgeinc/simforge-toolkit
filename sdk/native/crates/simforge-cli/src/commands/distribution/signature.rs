//! Distribution signatures (docs/cloud-api.md, Distribution signing).
//!
//! Every runtime, CLI and map-pack download names its archive by sha256 and byte count, and its
//! manifest carries an ed25519 signature over one message naming the artifact and those archive
//! bytes. Everything installed from the archive is bound to it (the runtime's inner manifest, a
//! map pack's `SHA256SUMS` and receipts), so the signature covers the whole install. The trusted
//! public keys are compiled in from `sdk/contracts/distribution-signing-keys.json`; each is bound
//! to the hosts of one channel, so a dev-signed artifact never installs from another host.

use crate::contract::CliError;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const KEYS: &str = include_str!("../../../../../../contracts/distribution-signing-keys.json");
const MESSAGE_SCHEMA: &str = "simforge.distribution-signed/v1";
const SIGNATURE_SCHEMA: &str = "simforge.distribution-signature/v1";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedKey {
    pub key_id: String,
    pub channel: String,
    pub hosts: Vec<String>,
    pub algorithm: String,
    pub public_key: String,
}

#[derive(Deserialize)]
struct KeyFile {
    schema: String,
    keys: Vec<TrustedKey>,
}

/// The artifact a signature names: what the server answered for this download.
pub struct Identity<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub target: Option<&'a str>,
    pub version: &'a str,
    pub sha256: &'a str,
    pub bytes: u64,
}

pub fn message(identity: &Identity) -> String {
    format!(
        "{MESSAGE_SCHEMA}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        identity.kind,
        identity.name,
        identity.target.unwrap_or("-"),
        identity.version,
        identity.sha256,
        identity.bytes
    )
}

/// The compiled-in trusted keys.
pub fn trusted_keys() -> Vec<TrustedKey> {
    let file: KeyFile =
        serde_json::from_str(KEYS).expect("distribution-signing-keys.json is valid");
    assert_eq!(file.schema, "simforge.distribution-signing-keys/v1");
    file.keys
}

fn refused(code: &str, reason: String, identity: &Identity, host: &str) -> CliError {
    CliError::new(code, reason).with_detail(json!({
        "kind": identity.kind, "name": identity.name, "version": identity.version, "host": host,
        "hint": "Nothing was installed. A SimForge download must carry a signature from a key bound to the host it came from (sdk/contracts/distribution-signing-keys.json); update the simforge CLI if this host's key is newer than it.",
    }))
}

/// Verify `signature` (the manifest's `signature` field) for a download from `host`.
pub fn verify(
    keys: &[TrustedKey],
    host: &str,
    identity: &Identity,
    signature: &Value,
) -> Result<(), CliError> {
    if signature.is_null() {
        return Err(refused(
            "signature_missing",
            format!(
                "{} {} {} is not signed",
                identity.kind, identity.name, identity.version
            ),
            identity,
            host,
        ));
    }
    let field = |key: &str| signature[key].as_str().unwrap_or_default();
    if field("schema") != SIGNATURE_SCHEMA || field("algorithm") != "ed25519" {
        return Err(refused(
            "signature_invalid",
            format!("the signature is not a {SIGNATURE_SCHEMA} ed25519 signature"),
            identity,
            host,
        ));
    }
    let key_id = field("keyId");
    let Some(key) = keys.iter().find(|k| k.key_id == key_id) else {
        return Err(refused(
            "signature_untrusted",
            format!("signing key {key_id:?} is not trusted by this CLI"),
            identity,
            host,
        ));
    };
    let base64 = base64::engine::general_purpose::STANDARD;
    let public = base64.decode(&key.public_key).unwrap_or_default();
    let digest = Sha256::digest(&public);
    let id: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    if key.algorithm != "ed25519" || public.len() != 32 || id != key.key_id {
        return Err(refused(
            "signature_untrusted",
            format!("trusted key {} is malformed", key.key_id),
            identity,
            host,
        ));
    }
    if !key.hosts.iter().any(|h| h == host) {
        return Err(refused(
            "signature_wrong_host",
            format!(
                "key {} signs {} downloads ({}), not downloads from {host}",
                key.key_id,
                key.channel,
                key.hosts.join(", ")
            ),
            identity,
            host,
        ));
    }
    let value = base64.decode(field("value")).unwrap_or_default();
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &public)
        .verify(message(identity).as_bytes(), &value)
        .map_err(|_| {
            refused(
                "signature_mismatch",
                format!(
                    "the signature does not cover {} {} {} with archive sha256 {}",
                    identity.kind, identity.name, identity.version, identity.sha256
                ),
                identity,
                host,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8032 section 7.1, test 1: the secret key whose public key is below.
    const TEST_SEED: [u8; 32] = [
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c,
        0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae,
        0x7f, 0x60,
    ];
    const TEST_PUBLIC: &str = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";

    fn key(hosts: &[&str]) -> TrustedKey {
        let public = base64::engine::general_purpose::STANDARD
            .decode(TEST_PUBLIC)
            .unwrap();
        let digest = Sha256::digest(&public);
        TrustedKey {
            key_id: digest[..8].iter().map(|b| format!("{b:02x}")).collect(),
            channel: "dev".into(),
            hosts: hosts.iter().map(|h| (*h).to_owned()).collect(),
            algorithm: "ed25519".into(),
            public_key: TEST_PUBLIC.into(),
        }
    }

    fn identity() -> Identity<'static> {
        Identity {
            kind: "runtime",
            name: "simforge-runtime",
            target: Some("x86_64-unknown-linux-gnu"),
            version: "0.1.0+0123456789abcdef",
            sha256: "ab3f1b0a8d0b6f1e3f0c9f7e1b2d4c6a8e0f2b4d6f8a0c2e4f6b8d0a2c4e6f81",
            bytes: 4096,
        }
    }

    fn sign(key: &TrustedKey, message: &str) -> Value {
        let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&TEST_SEED).unwrap();
        let value = base64::engine::general_purpose::STANDARD.encode(pair.sign(message.as_bytes()));
        json!({"schema": SIGNATURE_SCHEMA, "algorithm": "ed25519", "keyId": key.key_id, "value": value})
    }

    #[test]
    fn the_signed_message_names_the_artifact_and_its_archive_bytes() {
        assert_eq!(
            message(&identity()),
            "simforge.distribution-signed/v1\nruntime\nsimforge-runtime\nx86_64-unknown-linux-gnu\n0.1.0+0123456789abcdef\nab3f1b0a8d0b6f1e3f0c9f7e1b2d4c6a8e0f2b4d6f8a0c2e4f6b8d0a2c4e6f81\n4096\n"
        );
        let map = Identity {
            kind: "map",
            name: "richmond-field-station",
            target: None,
            version: "v9",
            sha256: "00",
            bytes: 1,
        };
        assert!(message(&map).contains("\nrichmond-field-station\n-\nv9\n"));
    }

    #[test]
    fn a_signature_verifies_only_for_its_artifact_and_a_host_its_key_is_bound_to() {
        let key = key(&["dev.simforge.ai"]);
        let signature = sign(&key, &message(&identity()));
        let keys = [key.clone()];
        verify(&keys, "dev.simforge.ai", &identity(), &signature).unwrap();
        let code = |host: &str, id: &Identity, signature: &Value| {
            verify(&keys, host, id, signature).unwrap_err().code
        };
        assert_eq!(
            code("simforge.ai", &identity(), &signature),
            "signature_wrong_host"
        );
        let other = Identity {
            bytes: 4097,
            ..identity()
        };
        assert_eq!(
            code("dev.simforge.ai", &other, &signature),
            "signature_mismatch"
        );
        assert_eq!(
            code("dev.simforge.ai", &identity(), &Value::Null),
            "signature_missing"
        );
        let mut untrusted = signature.clone();
        untrusted["keyId"] = json!("0000000000000000");
        assert_eq!(
            code("dev.simforge.ai", &identity(), &untrusted),
            "signature_untrusted"
        );
        let mut sha1 = signature;
        sha1["algorithm"] = json!("rsa");
        assert_eq!(
            code("dev.simforge.ai", &identity(), &sha1),
            "signature_invalid"
        );
    }

    #[test]
    fn the_compiled_in_keys_are_well_formed_and_bound_to_simforge_hosts() {
        for key in trusted_keys() {
            let public = base64::engine::general_purpose::STANDARD
                .decode(&key.public_key)
                .unwrap();
            let digest = Sha256::digest(&public);
            let id: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!((public.len(), id.as_str()), (32, key.key_id.as_str()));
            let expected: &[&str] = match key.channel.as_str() {
                "dev" => &["dev.simforge.ai"],
                "prod" => &["simforge.ai"],
                other => panic!("unknown channel {other}"),
            };
            assert_eq!(key.hosts, expected);
        }
    }
}
