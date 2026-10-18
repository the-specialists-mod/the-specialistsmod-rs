//! Trust levels and the built-in root keys (client-plugins.md §3.3).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::sign::{PublicKey, Signer};
use crate::store::Store;

/// The keys built into the port that may countersign publisher keys (`publisher.cert`): the
/// the-specialists-mod release key(s), as `ed25519:BASE64`. Both the client and tsmod-launcher
/// read this list, through this crate, so they agree on what "verified" means.
///
/// Empty until the countersigning key exists (open question 4), so for now no package is
/// "verified"; signed packages are "known" or "new" publishers.
pub const BUILTIN_ROOTS: &[&str] = &[];

/// [`BUILTIN_ROOTS`], parsed. An entry that doesn't parse is skipped (and fails this crate's
/// tests).
pub fn builtin_roots() -> Vec<PublicKey> {
    BUILTIN_ROOTS.iter().filter_map(|k| k.parse().ok()).collect()
}

/// A trust level, as shown on the prompts and cards and recorded in the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// Signed, and the key is countersigned by a built-in root.
    Verified,
    /// Signed by a key the player has approved a plugin under before.
    Known,
    /// Signed by a key the player hasn't approved before.
    New,
    /// No signature: a package without `manifest.sig`, or a hand-installed library.
    Unsigned,
}

impl Trust {
    /// Whether the plugin is signed at all.
    pub fn is_signed(self) -> bool {
        self != Trust::Unsigned
    }
}

/// A trust level with what the prompt needs to say about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustInfo {
    /// The level.
    pub trust: Trust,
    /// The publisher name: the countersigned one when verified, else the manifest's.
    pub publisher: Option<String>,
    /// The key's fingerprint, when signed.
    pub fingerprint: Option<String>,
    /// When a plugin under this key was first approved, for [`Trust::Known`].
    pub approved_before: Option<String>,
}

impl TrustInfo {
    /// The level of a plugin signed by `signer`, whose manifest names `publisher`, given what
    /// the player has approved before. Compute it before writing the new approval: afterwards
    /// every signed plugin looks known.
    pub fn assess(signer: &Signer, publisher: Option<&str>, store: &Store) -> TrustInfo {
        match signer {
            Signer::Unsigned => {
                TrustInfo { trust: Trust::Unsigned, publisher: publisher.map(Into::into), fingerprint: None, approved_before: None }
            }
            Signer::Signed { key, verified_as } => {
                let fingerprint = Some(key.fingerprint());
                if let Some(name) = verified_as {
                    return TrustInfo { trust: Trust::Verified, publisher: Some(name.clone()), fingerprint, approved_before: None };
                }
                let known = store.known_publisher(key).map(|e| e.approved_at.clone());
                let trust = if known.is_some() { Trust::Known } else { Trust::New };
                TrustInfo { trust, publisher: publisher.map(Into::into), fingerprint, approved_before: known }
            }
        }
    }
}

impl fmt::Display for TrustInfo {
    /// The line the prompts and cards show (§3.3's "Shown as").
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self.publisher.as_deref().unwrap_or("an unnamed publisher");
        match self.trust {
            Trust::Verified => write!(f, "Verified publisher: {name}"),
            Trust::Known => {
                let date = self.approved_before.as_deref().map_or("", |d| d.get(..10).unwrap_or(d));
                write!(f, "Signed by {name}, approved before on {date}")
            }
            Trust::New => write!(f, "Signed by an unknown publisher (key {})", self.fingerprint.as_deref().unwrap_or("?")),
            Trust::Unsigned => write!(f, "Unsigned: nobody vouches for this file"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Entry, Scope, State};
    use crate::test_util::key;

    #[test]
    fn builtin_roots_parse() {
        assert_eq!(builtin_roots().len(), BUILTIN_ROOTS.len());
    }

    #[test]
    fn levels() {
        let k = PublicKey::of(&key(1));
        let mut store = Store::new();
        let unsigned = TrustInfo::assess(&Signer::Unsigned, Some("Bob"), &store);
        assert_eq!(unsigned.trust, Trust::Unsigned);
        assert_eq!(unsigned.to_string(), "Unsigned: nobody vouches for this file");

        let signed = Signer::Signed { key: k, verified_as: None };
        let new = TrustInfo::assess(&signed, Some("Bob"), &store);
        assert_eq!(new.trust, Trust::New);
        assert!(new.to_string().contains(&k.fingerprint()));

        store.allow(Entry {
            id: "other".into(),
            version: "1.0.0".parse().unwrap(),
            state: State::Allowed,
            scope: Scope::Always,
            servers: vec![],
            source: "launcher".into(),
            publisher_key: Some(k),
            trust: Trust::New,
            approved_at: "2026-10-17T10:42:00Z".into(),
            manifest_sha256: None,
            platform: crate::Platform::LinuxX86_64,
            files: Default::default(),
        });
        let known = TrustInfo::assess(&signed, Some("Bob"), &store);
        assert_eq!(known.trust, Trust::Known);
        assert_eq!(known.to_string(), "Signed by Bob, approved before on 2026-10-17");

        let verified = TrustInfo::assess(&Signer::Signed { key: k, verified_as: Some("TSM".into()) }, Some("Bob"), &store);
        assert_eq!(verified.trust, Trust::Verified);
        assert_eq!(verified.to_string(), "Verified publisher: TSM", "the countersigned name, not the manifest's");
    }
}
