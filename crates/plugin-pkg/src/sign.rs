//! Manifest signatures and publisher countersignatures (client-plugins.md §3.3).
//!
//! * `manifest.sig` is the raw 64-byte ed25519 signature of `manifest.toml`'s exact bytes,
//!   made with the key in the manifest's `publisher.key`.
//! * `publisher.cert` is a small TOML file in which a built-in root key ([`crate::trust`])
//!   vouches for a publisher key and name:
//!
//!   ```toml
//!   format = 1
//!   key = "ed25519:BASE64..."      # the publisher key, as in the manifest
//!   name = "The Specialists Mod"   # the publisher name, as in the manifest
//!   signer = "ed25519:BASE64..."   # the root key that signed
//!   signature = "BASE64..."        # ed25519 over cert_message(key, name)
//!   ```
//!
//! Signatures are checked with ed25519-dalek's `verify_strict`, which refuses malleable
//! signatures and weak keys.

use std::fmt;
use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::hash::Sha256;
use crate::manifest::Manifest;

/// The largest `manifest.sig` or `publisher.cert` read from a package or the network.
pub const MAX_SIGNATURE_FILE_BYTES: u64 = 4096;

/// An ed25519 public key, written `ed25519:<standard base64 of the 32 bytes>`. Holds the 32
/// bytes, checked to be a valid, non-weak key when the value was made.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    /// The key's 32 bytes.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// The key from its 32 bytes. Refuses points that aren't valid keys, and weak keys.
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<PublicKey, String> {
        let key = VerifyingKey::from_bytes(bytes).map_err(|_| "not an ed25519 public key".to_string())?;
        if key.is_weak() {
            return Err("weak ed25519 key".into());
        }
        Ok(PublicKey(*bytes))
    }

    /// The public half of a signing key.
    pub fn of(secret: &SigningKey) -> PublicKey {
        PublicKey(secret.verifying_key().to_bytes())
    }

    /// The fingerprint the "new publisher" prompt shows: the first 16 bytes of the SHA-256 of
    /// the key, as 8 groups of 4 hex digits.
    pub fn fingerprint(&self) -> String {
        let hex = Sha256::of_bytes(&self.to_bytes()).to_hex();
        hex.as_bytes()[..32].chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join(" ")
    }

    /// Checks `signature` (exactly 64 bytes) over `message`.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        let Ok(bytes) = <[u8; 64]>::try_from(signature) else { return false };
        let Ok(key) = VerifyingKey::from_bytes(&self.0) else { return false };
        key.verify_strict(message, &Signature::from_bytes(&bytes)).is_ok()
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ed25519:{}", B64.encode(self.to_bytes()))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({self})")
    }
}

impl FromStr for PublicKey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let b64 = s.strip_prefix("ed25519:").ok_or_else(|| format!("a key starts with \"ed25519:\": {s:?}"))?;
        let raw = B64.decode(b64).map_err(|_| format!("key isn't base64: {s:?}"))?;
        let bytes = <[u8; 32]>::try_from(raw.as_slice()).map_err(|_| format!("an ed25519 key is 32 bytes: {s:?}"))?;
        PublicKey::from_bytes(&bytes)
    }
}

impl Serialize for PublicKey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Who vouches for a manifest, once its signatures have checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signer {
    /// No `manifest.sig`, or a hand-installed library.
    Unsigned,
    /// `manifest.sig` checks against `key`.
    Signed {
        /// The publisher key from the manifest.
        key: PublicKey,
        /// The publisher name a built-in root countersigned, when `publisher.cert` checks.
        verified_as: Option<String>,
    },
}

impl Signer {
    /// The publisher key, if signed.
    pub fn key(&self) -> Option<PublicKey> {
        match self {
            Signer::Unsigned => None,
            Signer::Signed { key, .. } => Some(*key),
        }
    }
}

/// Why a package's signatures are refused. Any of these rejects the package: a broken
/// signature is never treated as "unsigned".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureError {
    /// `manifest.sig` exists but the manifest has no `publisher.key`.
    NoKey,
    /// `manifest.sig` isn't 64 bytes.
    Malformed,
    /// `manifest.sig` doesn't check against `publisher.key`.
    Bad,
    /// `publisher.cert` without `manifest.sig`.
    CertWithoutSignature,
    /// `publisher.cert` doesn't parse.
    CertMalformed(String),
    /// `publisher.cert` names another key or publisher name than the manifest.
    CertMismatch,
    /// `publisher.cert` is signed by a built-in root, but the signature doesn't check.
    CertBad,
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignatureError::NoKey => write!(f, "manifest.sig without a publisher.key"),
            SignatureError::Malformed => write!(f, "manifest.sig isn't a 64-byte ed25519 signature"),
            SignatureError::Bad => write!(f, "manifest.sig doesn't match the manifest and publisher.key"),
            SignatureError::CertWithoutSignature => write!(f, "publisher.cert without manifest.sig"),
            SignatureError::CertMalformed(e) => write!(f, "publisher.cert doesn't parse: {e}"),
            SignatureError::CertMismatch => write!(f, "publisher.cert is for another publisher key or name"),
            SignatureError::CertBad => write!(f, "publisher.cert's signature doesn't check"),
        }
    }
}

impl std::error::Error for SignatureError {}

/// The bytes a root signs in `publisher.cert`. Domain-separated, and binding the name, so a
/// certificate can't be reused for another name or as a manifest signature.
pub fn cert_message(key: &PublicKey, name: &str) -> Vec<u8> {
    let mut m = b"tsmod publisher.cert v1\n".to_vec();
    m.extend_from_slice(&key.to_bytes());
    m.extend_from_slice(name.as_bytes());
    m
}

/// `publisher.cert`, parsed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cert {
    /// Always 1.
    pub format: u32,
    /// The publisher key.
    pub key: PublicKey,
    /// The publisher name.
    pub name: String,
    /// The root key that signed.
    pub signer: PublicKey,
    /// Base64 of the signature over [`cert_message`].
    pub signature: String,
}

impl Cert {
    /// Parses `publisher.cert`.
    pub fn parse(bytes: &[u8]) -> Result<Cert, SignatureError> {
        let text = std::str::from_utf8(bytes).map_err(|_| SignatureError::CertMalformed("not UTF-8".into()))?;
        let cert: Cert = toml::from_str(text).map_err(|e| SignatureError::CertMalformed(e.message().to_string()))?;
        if cert.format != 1 {
            return Err(SignatureError::CertMalformed(format!("format {} (this build reads 1)", cert.format)));
        }
        Ok(cert)
    }

    /// Makes a certificate: `root` vouches for `key` under `name`. For the release-repo
    /// workflow that countersigns publishers, and for tests.
    pub fn issue(root: &SigningKey, key: &PublicKey, name: &str) -> Cert {
        let sig = root.sign(&cert_message(key, name));
        Cert { format: 1, key: *key, name: name.into(), signer: PublicKey::of(root), signature: B64.encode(sig.to_bytes()) }
    }

    /// The certificate as `publisher.cert` text.
    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }
}

/// Signs `manifest.toml`'s bytes; the result is `manifest.sig`.
pub fn sign_manifest(key: &SigningKey, manifest_bytes: &[u8]) -> [u8; 64] {
    key.sign(manifest_bytes).to_bytes()
}

/// Checks a manifest's signature and countersignature.
///
/// `roots` are the built-in keys ([`crate::trust::builtin_roots`]). A certificate signed by a
/// key that isn't a root is ignored (the package is still signed, just not verified), so a
/// retired root doesn't break old packages; one signed by a root that doesn't check is an error.
pub fn check_signature(
    manifest: &Manifest,
    manifest_bytes: &[u8],
    signature: Option<&[u8]>,
    cert: Option<&[u8]>,
    roots: &[PublicKey],
) -> Result<Signer, SignatureError> {
    let Some(signature) = signature else {
        return if cert.is_some() { Err(SignatureError::CertWithoutSignature) } else { Ok(Signer::Unsigned) };
    };
    let publisher = manifest.publisher.as_ref();
    let key = publisher.and_then(|p| p.key).ok_or(SignatureError::NoKey)?;
    if signature.len() != 64 {
        return Err(SignatureError::Malformed);
    }
    if !key.verify(manifest_bytes, signature) {
        return Err(SignatureError::Bad);
    }
    let mut verified_as = None;
    if let Some(cert) = cert {
        let cert = Cert::parse(cert)?;
        let name = publisher.map(|p| p.name.as_str()).unwrap_or("");
        if cert.key != key || cert.name != name {
            return Err(SignatureError::CertMismatch);
        }
        if roots.contains(&cert.signer) {
            let sig = B64.decode(&cert.signature).map_err(|_| SignatureError::CertMalformed("signature isn't base64".into()))?;
            if !cert.signer.verify(&cert_message(&cert.key, &cert.name), &sig) {
                return Err(SignatureError::CertBad);
            }
            verified_as = Some(cert.name);
        }
    }
    Ok(Signer::Signed { key, verified_as })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{key, manifest_text, publisher_toml};

    fn manifest(text: &str) -> Manifest {
        Manifest::parse(text.as_bytes()).unwrap()
    }

    #[test]
    fn key_text_round_trip_and_fingerprint() {
        let k = PublicKey::of(&key(1));
        let s = k.to_string();
        assert!(s.starts_with("ed25519:"));
        assert_eq!(s.parse::<PublicKey>().unwrap(), k);
        assert!("ed25519:AAAA".parse::<PublicKey>().is_err());
        assert!("rsa:AAAA".parse::<PublicKey>().is_err());
        assert!(format!("ed25519:{}", B64.encode([0u8; 32])).parse::<PublicKey>().is_err(), "the identity point is weak");
        assert_eq!(k.fingerprint().len(), 8 * 4 + 7);
        assert_ne!(k.fingerprint(), PublicKey::of(&key(2)).fingerprint());
    }

    #[test]
    fn signature_checks() {
        let publisher = key(1);
        let text = manifest_text("demo", "1.0.0", &publisher_toml("Pub", Some(&publisher)), &[]);
        let m = manifest(&text);
        let sig = sign_manifest(&publisher, text.as_bytes());
        let pk = PublicKey::of(&publisher);

        assert_eq!(check_signature(&m, text.as_bytes(), None, None, &[]), Ok(Signer::Unsigned));
        assert_eq!(check_signature(&m, text.as_bytes(), Some(&sig), None, &[]), Ok(Signer::Signed { key: pk, verified_as: None }));

        // One changed byte in the manifest, or in the signature.
        let mut tampered = text.clone().into_bytes();
        let last = tampered.len() - 2;
        tampered[last] ^= 1;
        assert_eq!(check_signature(&m, &tampered, Some(&sig), None, &[]), Err(SignatureError::Bad));
        let mut bad_sig = sig;
        bad_sig[10] ^= 1;
        assert_eq!(check_signature(&m, text.as_bytes(), Some(&bad_sig), None, &[]), Err(SignatureError::Bad));
        assert_eq!(check_signature(&m, text.as_bytes(), Some(&sig[..63]), None, &[]), Err(SignatureError::Malformed));

        // Signed by another key than the manifest names.
        let other = sign_manifest(&key(2), text.as_bytes());
        assert_eq!(check_signature(&m, text.as_bytes(), Some(&other), None, &[]), Err(SignatureError::Bad));

        // A signature with no key to check it against.
        let unsigned_text = manifest_text("demo", "1.0.0", &publisher_toml("Pub", None), &[]);
        let um = manifest(&unsigned_text);
        let usig = sign_manifest(&publisher, unsigned_text.as_bytes());
        assert_eq!(check_signature(&um, unsigned_text.as_bytes(), Some(&usig), None, &[]), Err(SignatureError::NoKey));
    }

    #[test]
    fn countersignature_checks() {
        let root = key(9);
        let roots = [PublicKey::of(&root)];
        let publisher = key(1);
        let pk = PublicKey::of(&publisher);
        let text = manifest_text("demo", "1.0.0", &publisher_toml("Pub", Some(&publisher)), &[]);
        let m = manifest(&text);
        let sig = sign_manifest(&publisher, text.as_bytes());
        let cert = Cert::issue(&root, &pk, "Pub").to_toml();

        let verified = Signer::Signed { key: pk, verified_as: Some("Pub".into()) };
        assert_eq!(check_signature(&m, text.as_bytes(), Some(&sig), Some(cert.as_bytes()), &roots), Ok(verified));
        // Not one of this build's roots: signed, not verified.
        assert_eq!(
            check_signature(&m, text.as_bytes(), Some(&sig), Some(cert.as_bytes()), &[PublicKey::of(&key(8))]),
            Ok(Signer::Signed { key: pk, verified_as: None })
        );
        // A certificate for another name, another key, or with a broken signature.
        let renamed = Cert::issue(&root, &pk, "Someone else").to_toml();
        assert_eq!(check_signature(&m, text.as_bytes(), Some(&sig), Some(renamed.as_bytes()), &roots), Err(SignatureError::CertMismatch));
        let other_key = Cert::issue(&root, &PublicKey::of(&key(2)), "Pub").to_toml();
        assert_eq!(check_signature(&m, text.as_bytes(), Some(&sig), Some(other_key.as_bytes()), &roots), Err(SignatureError::CertMismatch));
        let mut forged = Cert::issue(&key(7), &pk, "Pub");
        forged.signer = roots[0];
        assert_eq!(
            check_signature(&m, text.as_bytes(), Some(&sig), Some(forged.to_toml().as_bytes()), &roots),
            Err(SignatureError::CertBad)
        );
        assert!(matches!(check_signature(&m, text.as_bytes(), Some(&sig), Some(b"junk"), &roots), Err(SignatureError::CertMalformed(_))));
        assert_eq!(check_signature(&m, text.as_bytes(), None, Some(cert.as_bytes()), &roots), Err(SignatureError::CertWithoutSignature));
        // A certificate signature can't stand in for a manifest signature.
        let cert_sig = B64.decode(Cert::issue(&publisher, &pk, "Pub").signature).unwrap();
        assert_eq!(check_signature(&m, text.as_bytes(), Some(&cert_sig), None, &roots), Err(SignatureError::Bad));
    }
}
