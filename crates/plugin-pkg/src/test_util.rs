//! Fixtures shared by the unit tests.

use std::fs;
use std::path::Path;

use ed25519_dalek::SigningKey;

use crate::hash::Sha256;
use crate::sign::{Cert, PublicKey, sign_manifest};

/// A fixed key, so failures reproduce.
pub fn key(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}

pub fn publisher_toml(name: &str, key: Option<&SigningKey>) -> String {
    let mut s = format!("[publisher]\nname = \"{name}\"\n");
    if let Some(k) = key {
        s.push_str(&format!("key = \"{}\"\n", PublicKey::of(k)));
    }
    s
}

pub fn file_toml(path: &str, size: u64, sha: &Sha256, platform: Option<&str>) -> String {
    let mut s = format!("[[files]]\npath = \"{path}\"\nsize = {size}\nsha256 = \"{sha}\"\n");
    if let Some(p) = platform {
        s.push_str(&format!("platform = \"{p}\"\n"));
    }
    s
}

/// A minimal manifest whose linux-x86_64 library is `lib/tsmod_<id>.so`. With no `files`, that
/// library is listed as an empty file.
pub fn manifest_text(id: &str, version: &str, publisher: &str, files: &[String]) -> String {
    let files = if files.is_empty() { file_toml(&format!("lib/tsmod_{id}.so"), 0, &Sha256::of_bytes(b""), None) } else { files.concat() };
    format!(
        "format = 1\nid = \"{id}\"\nname = \"Demo\"\nversion = \"{version}\"\nclient_ext_api = 1\n\n{publisher}\n[platforms]\nlinux-x86_64 = \"lib/tsmod_{id}.so\"\n\n[declares]\nnetwork = false\n\n{files}"
    )
}

/// A sample plugin's source files, manifest and signature files, ready to package.
pub struct Sample {
    pub src: tempfile::TempDir,
    pub manifest: String,
    pub sig: Option<[u8; 64]>,
    pub cert: Option<String>,
}

pub const SAMPLE_FILES: &[(&str, &[u8], Option<&str>)] = &[
    ("lib/linux-x86_64/tsmod_demo.so", b"linux library", Some("linux-x86_64")),
    ("lib/windows-i686/tsmod_demo.dll", b"windows library", Some("windows-i686")),
    ("bin/demo-host", b"helper program", None),
    ("data/readme.txt", b"hello", None),
];

/// The sample `demo` plugin at `version`, signed by `publisher` (countersigned by `root`) or
/// unsigned.
pub fn sample(version: &str, publisher: Option<&SigningKey>, root: Option<&SigningKey>) -> Sample {
    let src = tempfile::tempdir().unwrap();
    let mut files = String::new();
    for (path, bytes, platform) in SAMPLE_FILES {
        let full = src.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, bytes).unwrap();
        files.push_str(&file_toml(path, bytes.len() as u64, &Sha256::of_bytes(bytes), *platform));
    }
    let manifest = format!(
        "format = 1\nid = \"demo\"\nname = \"Demo\"\nversion = \"{version}\"\nclient_ext_api = 1\n\n{}\n[platforms]\n\
         linux-x86_64 = \"lib/linux-x86_64/tsmod_demo.so\"\nwindows-i686 = \"lib/windows-i686/tsmod_demo.dll\"\n\n\
         [declares]\nprocesses = [\"bin/demo-host\"]\n\n{files}",
        publisher_toml("Demo Pub", publisher)
    );
    let sig = publisher.map(|k| sign_manifest(k, manifest.as_bytes()));
    let cert = publisher.zip(root).map(|(k, r)| Cert::issue(r, &PublicKey::of(k), "Demo Pub").to_toml());
    Sample { src, manifest, sig, cert }
}

impl Sample {
    pub fn package_bytes(&self) -> Vec<u8> {
        let out = crate::build::write_package(
            std::io::Cursor::new(Vec::new()),
            self.manifest.as_bytes(),
            self.sig.as_ref().map(|s| &s[..]),
            self.cert.as_deref().map(str::as_bytes),
            self.src.path(),
        )
        .unwrap();
        out.into_inner()
    }

    pub fn write_package(&self, path: &Path) {
        fs::write(path, self.package_bytes()).unwrap();
    }
}
