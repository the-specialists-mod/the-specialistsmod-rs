//! Making packages: for `cargo xtask` (plugin packaging, `plugin-offer`, `plugin-approve`),
//! plugin repositories' release workflows, and tests.

use std::io::{self, Seek, Write};
use std::path::Path;

use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use crate::hash::Sha256;
use crate::manifest::{FileEntry, Manifest};
use crate::package::{CERT_FILE, FILES_DIR, MANIFEST_FILE, PackageError, SIGNATURE_FILE};
use crate::platform::Platform;

/// A `[[files]]` entry for `root/<path>`, with its size and hash.
pub fn file_entry(root: &Path, path: &str, platform: Option<Platform>) -> io::Result<FileEntry> {
    let (sha256, size) = Sha256::of_file(&path.split('/').fold(root.to_path_buf(), |p, s| p.join(s)))?;
    Ok(FileEntry { path: path.into(), size, sha256, platform })
}

/// Writes a package: `manifest_bytes` as `manifest.toml` (sign exactly these bytes), the
/// signature files if given, and each manifest file from `files_root/<path>` as
/// `files/<path>`. Each file is checked against the manifest first, so this can't produce a
/// package that a client would refuse for its contents.
pub fn write_package<W: Write + Seek>(
    out: W,
    manifest_bytes: &[u8],
    signature: Option<&[u8]>,
    cert: Option<&[u8]>,
    files_root: &Path,
) -> Result<W, PackageError> {
    let manifest = Manifest::parse(manifest_bytes).map_err(PackageError::Manifest)?;
    let zip_err = |e: zip::result::ZipError| PackageError::Zip(e.to_string());
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).unix_permissions(0o644);
    let mut zip = ZipWriter::new(out);
    zip.start_file(MANIFEST_FILE, opts).map_err(zip_err)?;
    zip.write_all(manifest_bytes)?;
    for (name, bytes) in [(SIGNATURE_FILE, signature), (CERT_FILE, cert)] {
        if let Some(bytes) = bytes {
            zip.start_file(name, opts).map_err(zip_err)?;
            zip.write_all(bytes)?;
        }
    }
    for f in &manifest.files {
        let bytes = std::fs::read(f.path.split('/').fold(files_root.to_path_buf(), |p, s| p.join(s)))?;
        if bytes.len() as u64 != f.size {
            return Err(PackageError::Size { path: f.path.clone(), expected: f.size, found: bytes.len() as u64 });
        }
        if Sha256::of_bytes(&bytes) != f.sha256 {
            return Err(PackageError::Hash(f.path.clone()));
        }
        zip.start_file(format!("{FILES_DIR}/{}", f.path), opts).map_err(zip_err)?;
        zip.write_all(&bytes)?;
    }
    zip.finish().map_err(zip_err)
}
