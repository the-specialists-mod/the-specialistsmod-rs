//! The checks a package passes before the server offers it (§6.1): the client would refuse a
//! package that fails any of them, so the server doesn't offer it at all, and logs why.
//!
//! The package on the server's disk (`Offer::local`, under the game directory) must have the
//! offer's size and SHA-256; it must open as a package whose zip entries are exactly its
//! manifest's files, with every file's size and hash matching; its manifest must name the
//! offer's id and version; and its signature, if any, must check. The files the client fetches
//! before the package, `<package without .tspkg>.manifest.toml` (and `.manifest.sig`,
//! `.publisher.cert` when the package has them), must be beside it and byte-identical to the
//! package's own.

use super::config::Offer;
use plugin_pkg::{Package, PublicKey, Sha256, Signer};
use std::fs;
use std::path::{Path, PathBuf};

/// What the server learnt from a package that passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// The manifest's name and homepage, for the kick reason of a refused required offer.
    pub name: String,
    pub homepage: Option<String>,
    /// "unsigned", "signed (<fingerprint>)" or "verified (<publisher>)", for the log.
    pub signer: String,
}

/// The sibling files a client fetches before the package, beside `<base>.tspkg`.
pub const SIBLINGS: [(&str, Sibling); 3] =
    [(".manifest.toml", Sibling::Manifest), (".manifest.sig", Sibling::Signature), (".publisher.cert", Sibling::Cert)];

/// Which of the package's files a sibling copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sibling {
    Manifest,
    Signature,
    Cert,
}

/// `<dir>/<base>.manifest.toml` etc. for the package at `path`.
pub fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let base = name.strip_suffix(plugin_pkg::package::PACKAGE_EXTENSION).unwrap_or(&name);
    path.with_file_name(format!("{base}{suffix}"))
}

/// The package's bytes for `which`, as its siblings must hold them.
pub fn sibling_bytes<R: std::io::Read + std::io::Seek>(p: &Package<R>, which: Sibling) -> Option<&[u8]> {
    match which {
        Sibling::Manifest => Some(p.manifest_bytes()),
        Sibling::Signature => p.signature_bytes(),
        Sibling::Cert => p.cert_bytes(),
    }
}

/// Runs every check on `offer`'s package under `game_dir`.
pub fn check(game_dir: &Path, offer: &Offer, roots: &[PublicKey]) -> Result<Checked, String> {
    let path = offer.local.split('/').fold(game_dir.to_path_buf(), |p, s| p.join(s));
    let shown = offer.local.as_str();
    let (sha, size) = Sha256::of_file(&path).map_err(|e| format!("{shown}: {e}"))?;
    if size != u64::from(offer.size) {
        return Err(format!("{shown} is {size} bytes, the offer says {}", offer.size));
    }
    if sha != offer.sha256 {
        return Err(format!("{shown}'s SHA-256 is {sha}, the offer says {}", offer.sha256));
    }
    let mut p = Package::open(&path).map_err(|e| format!("{shown}: {e}"))?;
    let m = p.manifest();
    if m.id != offer.id || m.version != offer.version {
        return Err(format!("{shown} is {} {}, the offer says {} {}", m.id, m.version, offer.id, offer.version));
    }
    p.verify_contents().map_err(|e| format!("{shown}: {e}"))?;
    let signer = p.check_signature(roots).map_err(|e| format!("{shown}: {e}"))?;
    for (suffix, which) in SIBLINGS {
        let sib = sibling_path(&path, suffix);
        let shown_sib = sib.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match (sibling_bytes(&p, which), fs::read(&sib)) {
            (Some(want), Ok(have)) if have == want => {}
            (Some(_), Ok(_)) => {
                return Err(format!("{shown_sib} differs from the package's own (rewrite it with cargo xtask plugin-offer)"));
            }
            (Some(_), Err(e)) => return Err(format!("{shown_sib}: {e} (the client fetches it first; cargo xtask plugin-offer writes it)")),
            (None, Ok(_)) => return Err(format!("{shown_sib} is there, but the package has no such file")),
            (None, Err(_)) => {}
        }
    }
    let m = p.manifest();
    let signer = match signer {
        Signer::Unsigned => "unsigned".to_owned(),
        Signer::Signed { verified_as: Some(name), .. } => format!("verified ({name})"),
        Signer::Signed { key, verified_as: None } => format!("signed ({})", key.fingerprint()),
    };
    Ok(Checked { name: m.name.clone(), homepage: m.homepage.clone(), signer })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_offers::config;
    use plugin_pkg::SigningKey;

    struct Fixture {
        dir: tempfile::TempDir,
        offer: Offer,
    }

    /// A package `demo` 0.1.0 at `plugins/demo-0.1.0.tspkg` in a game directory, with its
    /// siblings, and the offer that matches it.
    fn fixture(signed: bool) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("lib")).unwrap();
        fs::write(src.join("lib/tsmod_demo.so"), b"library").unwrap();
        let key = SigningKey::from_bytes(&[7; 32]);
        let publisher = if signed { format!("[publisher]\nname = \"Demo\"\nkey = \"{}\"\n", PublicKey::of(&key)) } else { String::new() };
        let manifest = format!(
            "format = 1\nid = \"demo\"\nname = \"Demo plugin\"\nversion = \"0.1.0\"\nhomepage = \"https://example.org/demo\"\n\
             client_ext_api = 1\n{publisher}\n[platforms]\nlinux-x86_64 = \"lib/tsmod_demo.so\"\n\n[[files]]\npath = \"lib/tsmod_demo.so\"\n\
             size = 7\nsha256 = \"{}\"\n",
            Sha256::of_bytes(b"library")
        );
        let sig = signed.then(|| plugin_pkg::sign_manifest(&key, manifest.as_bytes()));
        let plugins = dir.path().join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        let pkg = plugins.join("demo-0.1.0.tspkg");
        let file = fs::File::create(&pkg).unwrap();
        plugin_pkg::build::write_package(file, manifest.as_bytes(), sig.as_ref().map(|s| &s[..]), None, &src).unwrap();
        fs::write(plugins.join("demo-0.1.0.manifest.toml"), &manifest).unwrap();
        if let Some(s) = sig {
            fs::write(plugins.join("demo-0.1.0.manifest.sig"), s).unwrap();
        }
        let (sha, size) = Sha256::of_file(&pkg).unwrap();
        let (o, errors) = config::parse(&format!(
            "[[offer]]\nid = \"demo\"\nversion = \"0.1.0\"\nurl = \"plugins/demo-0.1.0.tspkg\"\nsha256 = \"{sha}\"\nsize = {size}\n"
        ));
        assert!(errors.is_empty(), "{errors:?}");
        Fixture { dir, offer: o.offers[0].clone() }
    }

    #[test]
    fn a_good_package_passes() {
        for signed in [false, true] {
            let f = fixture(signed);
            let c = check(f.dir.path(), &f.offer, &[]).unwrap();
            assert_eq!(c.name, "Demo plugin");
            assert_eq!(c.homepage.as_deref(), Some("https://example.org/demo"));
            assert_eq!(c.signer.starts_with("signed ("), signed, "{}", c.signer);
        }
    }

    #[test]
    fn mismatches_are_refused() {
        let f = fixture(true);
        let err = |o: &Offer| check(f.dir.path(), o, &[]).unwrap_err();
        let mut o = f.offer.clone();
        o.size += 1;
        assert!(err(&o).contains("bytes"));
        let mut o = f.offer.clone();
        o.sha256 = Sha256([0; 32]);
        assert!(err(&o).contains("SHA-256"));
        let mut o = f.offer.clone();
        o.version = plugin_pkg::Version::new(0, 2, 0);
        assert!(err(&o).contains("the offer says demo 0.2.0"));
        let mut o = f.offer.clone();
        o.local = "plugins/missing.tspkg".into();
        assert!(err(&o).contains("missing.tspkg"));

        let plugins = f.dir.path().join("plugins");
        fs::write(plugins.join("demo-0.1.0.manifest.toml"), b"format = 1\n").unwrap();
        assert!(err(&f.offer).contains("differs"));
        fs::remove_file(plugins.join("demo-0.1.0.manifest.toml")).unwrap();
        assert!(err(&f.offer).contains("fetches it first"));
    }

    #[test]
    fn a_stray_sibling_or_missing_signature_is_refused() {
        let f = fixture(false);
        let plugins = f.dir.path().join("plugins");
        fs::write(plugins.join("demo-0.1.0.manifest.sig"), [0u8; 64]).unwrap();
        assert!(check(f.dir.path(), &f.offer, &[]).unwrap_err().contains("no such file"));
        let f = fixture(true);
        fs::remove_file(f.dir.path().join("plugins/demo-0.1.0.manifest.sig")).unwrap();
        assert!(check(f.dir.path(), &f.offer, &[]).unwrap_err().contains("manifest.sig"));
    }

    #[test]
    fn a_tampered_package_is_refused() {
        // A file whose bytes don't match the manifest: the offer's hash is the tampered
        // package's, so only the content check catches it.
        let f = fixture(false);
        let pkg = f.dir.path().join("plugins/demo-0.1.0.tspkg");
        let mut bytes = fs::read(&pkg).unwrap();
        let at = bytes.windows(7).position(|w| w == b"library");
        // Deflate may not keep the text; then make the zip unreadable instead.
        match at {
            Some(i) => bytes[i] = b'L',
            None => bytes.truncate(bytes.len() / 2),
        }
        fs::write(&pkg, &bytes).unwrap();
        let (sha, size) = Sha256::of_file(&pkg).unwrap();
        let mut o = f.offer.clone();
        (o.sha256, o.size) = (sha, size as u32);
        assert!(check(f.dir.path(), &o, &[]).is_err());
        // A signature that doesn't check.
        let f = fixture(true);
        let sig = f.dir.path().join("plugins/demo-0.1.0.manifest.sig");
        let src = f.dir.path().join("src");
        let manifest = fs::read(f.dir.path().join("plugins/demo-0.1.0.manifest.toml")).unwrap();
        let pkg = f.dir.path().join("plugins/demo-0.1.0.tspkg");
        plugin_pkg::build::write_package(fs::File::create(&pkg).unwrap(), &manifest, Some(&[1u8; 64]), None, &src).unwrap();
        fs::write(&sig, [1u8; 64]).unwrap();
        let (sha, size) = Sha256::of_file(&pkg).unwrap();
        let mut o = f.offer.clone();
        (o.sha256, o.size) = (sha, size as u32);
        assert!(check(f.dir.path(), &o, &[]).unwrap_err().contains("manifest.sig doesn't match"));
    }

    #[test]
    fn sibling_paths() {
        assert_eq!(sibling_path(Path::new("a/b/x-1.0.0.tspkg"), ".manifest.toml"), Path::new("a/b/x-1.0.0.manifest.toml"));
    }
}
