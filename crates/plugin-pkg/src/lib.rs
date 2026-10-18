//! Client plugin packages and approval: the pure half of
//! `docs/implementation/client-plugins.md` (§3, §5, §10).
//!
//! The port client (`client.dll`) and tsmod-launcher both use this crate, so they agree on what
//! a package is, what the player approved and whether it still matches. It has no `unsafe` and
//! no engine dependency.
//!
//! The rule it enforces: **nothing loads without approval, and an approval covers exact
//! bytes.** `approved.toml` records a SHA-256 of every file; every start re-hashes every file
//! in full, and any changed, missing or extra file is a [`Change`] that stops the plugin
//! loading until the player approves again.
//!
//! # At start (the loader, and the launcher before it starts the game)
//!
//! ```no_run
//! use plugin_pkg::{Platform, Store, Verdict, builtin_roots, store_path, verify_all};
//! # fn main() -> std::io::Result<()> {
//! let (cl_plugins, cl_dlls) = (std::path::Path::new("ts/cl_plugins"), std::path::Path::new("ts/cl_dlls"));
//! let platform = Platform::current().expect("a supported platform");
//! let (store, _status) = Store::load(&store_path(cl_plugins))?; // a bad store is set aside as .bad
//! for v in verify_all(cl_plugins, cl_dlls, platform.library_suffix(), platform, &store, &builtin_roots(), 0)? {
//!     match v.verdict {
//!         Verdict::Load => { /* map v.library */ }
//!         Verdict::Prompt => { /* ask; on Allow: store.allow(v.approval(..)), then save */ }
//!         Verdict::Blocked | Verdict::Refuse => { /* log v.changes */ }
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! # A server offer (§6.3)
//!
//! Fetch and [`Manifest::parse`] the manifest, [`check_signature`] it, show the card with its
//! [`TrustInfo`] (and [`VersionChange`] against an approved version), download, check the
//! package's size and [`Sha256::of_file`] against the offer, [`Package::open`] it, compare
//! [`Package::manifest_bytes`] with the card's, [`Package::extract`] it to
//! `cl_plugins/<id>/<version>/`, then [`verify()`] the installed [`Candidate::Package`] and write
//! its [`Verification::approval`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod build;
pub mod hash;
pub mod manifest;
pub mod package;
pub mod path;
pub mod platform;
pub mod sign;
pub mod store;
pub mod time;
pub mod trust;
pub mod verify;
pub mod version;

pub use ed25519_dalek::SigningKey;
pub use hash::Sha256;
pub use manifest::{Declares, FileEntry, Manifest, ManifestError, Publisher, Requires};
pub use package::{FILES_DIR, Package, PackageError};
pub use path::{PathError, check_path, check_path_set};
pub use platform::Platform;
pub use semver::{Version, VersionReq};
pub use sign::{Cert, PublicKey, SignatureError, Signer, check_signature, sign_manifest};
pub use store::{Entry, LoadStatus, STORE_FILE, Scope, State, Store};
pub use trust::{BUILTIN_ROOTS, Trust, TrustInfo, builtin_roots};
pub use verify::{Candidate, Change, Verdict, Verification, change_lines, discover, store_path, verify, verify_all};
pub use version::{UpdateSummary, VersionChange};

#[cfg(test)]
pub(crate) mod test_util;
