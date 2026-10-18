//! `.tspkg` packages: zip checks, verification and unpacking (client-plugins.md §3.1, §6.3).
//!
//! A package holds exactly `manifest.toml`, optionally `manifest.sig` and `publisher.cert`, and
//! `files/<path>` for every `[[files]]` entry, plus directory entries for those files'
//! directories. Anything else rejects it: unlisted or missing files, symlinks and other special
//! entries, encrypted entries, compression other than stored or deflate, names that break the
//! path rules, duplicate names (case-insensitively), and local headers that disagree with the
//! central directory.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use sha2::Digest;
use zip::{CompressionMethod, ZipArchive};

use crate::hash::{Sha256, hash_files};
use crate::manifest::{MAX_MANIFEST_BYTES, Manifest, ManifestError};
use crate::path::{PathError, check_path};
use crate::platform::Platform;
use crate::sign::{MAX_SIGNATURE_FILE_BYTES, PublicKey, SignatureError, Signer, check_signature};
use crate::verify::{expected_files, scan_dir};

/// The manifest's name, in a package and an installed package directory.
pub const MANIFEST_FILE: &str = "manifest.toml";
/// The manifest signature's name.
pub const SIGNATURE_FILE: &str = "manifest.sig";
/// The countersignature's name.
pub const CERT_FILE: &str = "publisher.cert";
/// The directory holding the plugin's files, in a package and an installed package directory.
pub const FILES_DIR: &str = "files";
/// A package's file extension.
pub const PACKAGE_EXTENSION: &str = ".tspkg";

/// Why a package is refused.
#[derive(Debug)]
pub enum PackageError {
    /// Reading or writing failed.
    Io(io::Error),
    /// Not a readable zip, or its headers are inconsistent (duplicate names included).
    Zip(String),
    /// No `manifest.toml`.
    NoManifest,
    /// The manifest is refused.
    Manifest(ManifestError),
    /// An entry name that isn't one of the package's names.
    UnexpectedEntry(String),
    /// An entry name that breaks the path rules.
    BadName(String, PathError),
    /// An entry name twice, ignoring case.
    Duplicate(String),
    /// A symlink or other special entry.
    Special(String),
    /// An encrypted entry.
    Encrypted(String),
    /// Compression other than stored or deflate.
    Compression(String),
    /// A manifest file with no entry.
    Missing(String),
    /// An entry the manifest doesn't list.
    Extra(String),
    /// An entry's size isn't the manifest's.
    Size {
        /// The file.
        path: String,
        /// The manifest's size.
        expected: u64,
        /// The entry's (at most one byte past `expected`: reading stops there).
        found: u64,
    },
    /// An entry's hash isn't the manifest's.
    Hash(String),
    /// The signature or countersignature doesn't check.
    Signature(SignatureError),
    /// No library for this platform.
    Unsupported(Platform),
    /// The install directory already exists.
    Exists(PathBuf),
    /// After unpacking, the files on disk don't match the manifest.
    Installed(Vec<String>),
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageError::Io(e) => write!(f, "{e}"),
            PackageError::Zip(e) => write!(f, "not a valid package: {e}"),
            PackageError::NoManifest => write!(f, "no {MANIFEST_FILE}"),
            PackageError::Manifest(e) => write!(f, "{e}"),
            PackageError::UnexpectedEntry(n) => write!(f, "unexpected entry {n:?}"),
            PackageError::BadName(n, e) => write!(f, "entry {n:?}: {e}"),
            PackageError::Duplicate(n) => write!(f, "entry {n:?} appears twice"),
            PackageError::Special(n) => write!(f, "entry {n:?} is a symlink or special file"),
            PackageError::Encrypted(n) => write!(f, "entry {n:?} is encrypted"),
            PackageError::Compression(n) => write!(f, "entry {n:?} uses unsupported compression"),
            PackageError::Missing(p) => write!(f, "{p} is in the manifest but not the package"),
            PackageError::Extra(p) => write!(f, "{p} is in the package but not the manifest"),
            PackageError::Size { path, expected, found } => write!(f, "{path} is {found} bytes, the manifest says {expected}"),
            PackageError::Hash(p) => write!(f, "{p} doesn't match its hash in the manifest"),
            PackageError::Signature(e) => write!(f, "{e}"),
            PackageError::Unsupported(p) => write!(f, "no library for {p}"),
            PackageError::Exists(p) => write!(f, "{} already exists", p.display()),
            PackageError::Installed(paths) => write!(f, "unpacked files don't match the manifest: {}", paths.join(", ")),
        }
    }
}

impl std::error::Error for PackageError {}

impl From<io::Error> for PackageError {
    fn from(e: io::Error) -> Self {
        PackageError::Io(e)
    }
}

fn zip_err(e: zip::result::ZipError) -> PackageError {
    match e {
        zip::result::ZipError::Io(e) => PackageError::Io(e),
        e => PackageError::Zip(e.to_string()),
    }
}

/// An opened package whose entry list matches its manifest. File contents are checked by
/// [`Package::verify_contents`] or [`Package::extract`].
pub struct Package<R: Read + Seek> {
    archive: ZipArchive<R>,
    manifest: Manifest,
    manifest_bytes: Vec<u8>,
    signature: Option<Vec<u8>>,
    cert: Option<Vec<u8>>,
    /// The zip index of each `manifest.files` entry, in manifest order.
    entries: Vec<usize>,
}

impl Package<File> {
    /// Opens a `.tspkg` file.
    pub fn open(path: &Path) -> Result<Package<File>, PackageError> {
        Package::from_reader(File::open(path)?)
    }
}

/// An entry's type from its Unix mode, when it has one. Zips made on Windows have none.
fn entry_is_special(mode: Option<u32>, dir: bool) -> bool {
    match mode.map(|m| m & 0o170_000) {
        None | Some(0) => false,
        Some(0o040_000) => !dir,
        Some(0o100_000) => dir,
        Some(_) => true,
    }
}

impl<R: Read + Seek> Package<R> {
    /// Reads a package's entry list, manifest and signature files, and checks the entries
    /// against the manifest.
    pub fn from_reader(reader: R) -> Result<Package<R>, PackageError> {
        let mut archive = ZipArchive::new(reader).map_err(zip_err)?;
        archive.validate_header_consistency().map_err(zip_err)?;
        let mut names = HashSet::new();
        let mut special: [Option<usize>; 3] = [None; 3];
        let mut files: BTreeMap<String, usize> = BTreeMap::new();
        let mut dirs = Vec::new();
        for i in 0..archive.len() {
            let e = archive.by_index_raw(i).map_err(zip_err)?;
            let Ok(name) = std::str::from_utf8(e.name_raw()).map(str::to_string) else {
                return Err(PackageError::UnexpectedEntry(String::from_utf8_lossy(e.name_raw()).into_owned()));
            };
            let dir = e.is_dir();
            if e.encrypted() {
                return Err(PackageError::Encrypted(name));
            }
            if e.is_symlink() || entry_is_special(e.unix_mode(), dir) {
                return Err(PackageError::Special(name));
            }
            if !dir && !matches!(e.compression(), CompressionMethod::Stored | CompressionMethod::Deflated) {
                return Err(PackageError::Compression(name));
            }
            let bare = if dir { name.strip_suffix('/').unwrap_or(&name) } else { &name };
            if !names.insert(bare.to_ascii_lowercase()) {
                return Err(PackageError::Duplicate(name));
            }
            let rest = bare.strip_prefix("files/");
            if dir {
                if bare != FILES_DIR {
                    let rest = rest.ok_or_else(|| PackageError::UnexpectedEntry(name.clone()))?;
                    check_path(rest).map_err(|err| PackageError::BadName(name.clone(), err))?;
                    dirs.push((name.clone(), rest.to_string()));
                }
                continue;
            }
            match bare {
                MANIFEST_FILE => special[0] = Some(i),
                SIGNATURE_FILE => special[1] = Some(i),
                CERT_FILE => special[2] = Some(i),
                _ => {
                    let rest = rest.ok_or_else(|| PackageError::UnexpectedEntry(name.clone()))?;
                    check_path(rest).map_err(|err| PackageError::BadName(name.clone(), err))?;
                    files.insert(rest.to_string(), i);
                }
            }
        }

        let manifest_index = special[0].ok_or(PackageError::NoManifest)?;
        let manifest_bytes = read_entry_limited(&mut archive, manifest_index, MAX_MANIFEST_BYTES).map_err(|e| {
            if let PackageError::Size { found, .. } = e { PackageError::Manifest(ManifestError::TooLarge(found)) } else { e }
        })?;
        let manifest = Manifest::parse(&manifest_bytes).map_err(PackageError::Manifest)?;
        let signature = special[1].map(|i| read_entry_limited(&mut archive, i, MAX_SIGNATURE_FILE_BYTES)).transpose()?;
        let cert = special[2].map(|i| read_entry_limited(&mut archive, i, MAX_SIGNATURE_FILE_BYTES)).transpose()?;

        let mut entries = Vec::with_capacity(manifest.files.len());
        for f in &manifest.files {
            entries.push(files.remove(&f.path).ok_or_else(|| PackageError::Missing(f.path.clone()))?);
        }
        if let Some(extra) = files.into_keys().next() {
            return Err(PackageError::Extra(extra));
        }
        for (name, d) in dirs {
            let prefix = format!("{d}/");
            if !manifest.files.iter().any(|f| f.path.starts_with(&prefix)) {
                return Err(PackageError::UnexpectedEntry(name));
            }
        }
        Ok(Package { archive, manifest, manifest_bytes, signature, cert, entries })
    }

    /// The manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// `manifest.toml`'s exact bytes. The offer flow checks they're identical to the manifest
    /// shown on the card (§6.3 step 5).
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// SHA-256 of `manifest.toml`.
    pub fn manifest_sha256(&self) -> Sha256 {
        Sha256::of_bytes(&self.manifest_bytes)
    }

    /// `manifest.sig`, if present.
    pub fn signature_bytes(&self) -> Option<&[u8]> {
        self.signature.as_deref()
    }

    /// `publisher.cert`, if present.
    pub fn cert_bytes(&self) -> Option<&[u8]> {
        self.cert.as_deref()
    }

    /// Checks the signature and countersignature against `roots`.
    pub fn check_signature(&self, roots: &[PublicKey]) -> Result<Signer, SignatureError> {
        check_signature(&self.manifest, &self.manifest_bytes, self.signature.as_deref(), self.cert.as_deref(), roots)
    }

    /// Reads every file entry and checks its size and hash against the manifest.
    pub fn verify_contents(&mut self) -> Result<(), PackageError> {
        for k in 0..self.entries.len() {
            self.copy_entry(k, &mut io::sink())?;
        }
        Ok(())
    }

    /// Streams manifest file `k` into `out`, checking its size and hash.
    fn copy_entry(&mut self, k: usize, out: &mut dyn Write) -> Result<(), PackageError> {
        let f = &self.manifest.files[k];
        let entry = self.archive.by_index(self.entries[k]).map_err(zip_err)?;
        let mut limited = entry.take(f.size + 1);
        let mut hasher = sha2::Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = match limited.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // zip reports CRC and inflate errors as io errors.
                Err(e) => return Err(PackageError::Zip(format!("{}: {e}", f.path))),
            };
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            total += n as u64;
        }
        if total != f.size {
            return Err(PackageError::Size { path: f.path.clone(), expected: f.size, found: total });
        }
        if Sha256(hasher.finalize().into()) != f.sha256 {
            return Err(PackageError::Hash(f.path.clone()));
        }
        Ok(())
    }

    /// Unpacks the package for `platform` into `dest` (`cl_plugins/<id>/<version>/`), which
    /// must not exist yet (§6.3 step 6).
    ///
    /// Every entry is checked, including other platforms' files, which aren't written. The
    /// files go to a hidden sibling directory first, which is renamed into place, so `dest`
    /// never holds a partial install. Then the unpacked files are listed and re-hashed, and
    /// must match the manifest exactly. On any failure nothing is left behind. Returns the
    /// installed files and their hashes, relative to `dest`.
    pub fn extract(&mut self, dest: &Path, platform: Platform) -> Result<BTreeMap<String, Sha256>, PackageError> {
        if !self.manifest.supports(platform) {
            return Err(PackageError::Unsupported(platform));
        }
        if fs::symlink_metadata(dest).is_ok() {
            return Err(PackageError::Exists(dest.to_path_buf()));
        }
        let parent = dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let leaf = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tmp = parent.join(format!(".{leaf}.partial"));
        if fs::symlink_metadata(&tmp).is_ok() {
            fs::remove_dir_all(&tmp)?;
        }
        fs::create_dir(&tmp)?;
        if let Err(e) = self.unpack_into(&tmp, platform) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
        if let Err(e) = fs::rename(&tmp, dest) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e.into());
        }
        match self.check_installed(dest, platform) {
            Ok(files) => Ok(files),
            Err(e) => {
                let _ = fs::remove_dir_all(dest);
                Err(e)
            }
        }
    }

    fn unpack_into(&mut self, dir: &Path, platform: Platform) -> Result<(), PackageError> {
        fs::write(dir.join(MANIFEST_FILE), &self.manifest_bytes)?;
        if let Some(s) = &self.signature {
            fs::write(dir.join(SIGNATURE_FILE), s)?;
        }
        if let Some(c) = &self.cert {
            fs::write(dir.join(CERT_FILE), c)?;
        }
        let files_dir = dir.join(FILES_DIR);
        fs::create_dir(&files_dir)?;
        for k in 0..self.entries.len() {
            let f = &self.manifest.files[k];
            if !f.is_for(platform) {
                self.copy_entry(k, &mut io::sink())?;
                continue;
            }
            // The path rules already hold; joining segment by segment keeps it inside `dir`
            // whatever the host's separator.
            let target = f.path.split('/').fold(files_dir.clone(), |p, s| p.join(s));
            if let Some(p) = target.parent() {
                fs::create_dir_all(p)?;
            }
            let mut out = io::BufWriter::new(File::create_new(&target)?);
            self.copy_entry(k, &mut out)?;
            let file = out.into_inner().map_err(|e| e.into_error())?;
            file.sync_all()?;
            // The zip's own modes are ignored; only the declared processes become executable.
            #[cfg(unix)]
            if self.manifest.declares.processes.iter().any(|p| *p == self.manifest.files[k].path) {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o755))?;
            }
        }
        Ok(())
    }

    fn check_installed(&self, dir: &Path, platform: Platform) -> Result<BTreeMap<String, Sha256>, PackageError> {
        let mut meta = vec![(MANIFEST_FILE, self.manifest_sha256())];
        if let Some(s) = &self.signature {
            meta.push((SIGNATURE_FILE, Sha256::of_bytes(s)));
        }
        if let Some(c) = &self.cert {
            meta.push((CERT_FILE, Sha256::of_bytes(c)));
        }
        let expected = expected_files(&self.manifest, platform, &meta);
        let (found, special) = scan_dir(dir)?;
        let mut files = BTreeMap::new();
        let mut bad = special;
        for (rel, r) in found.iter().zip(hash_files(dir, &found, 0)) {
            match r {
                Ok((h, _)) if expected.get(rel) == Some(&h) => {
                    files.insert(rel.clone(), h);
                }
                _ => bad.push(rel.clone()),
            }
        }
        bad.extend(expected.keys().filter(|p| !files.contains_key(*p) && !found.contains(p)).cloned());
        if bad.is_empty() { Ok(files) } else { Err(PackageError::Installed(bad)) }
    }
}

/// Reads entry `index` whole, refusing more than `limit` bytes.
fn read_entry_limited<R: Read + Seek>(archive: &mut ZipArchive<R>, index: usize, limit: u64) -> Result<Vec<u8>, PackageError> {
    let entry = archive.by_index(index).map_err(zip_err)?;
    let name = entry.name().map(|n| n.to_string()).unwrap_or_default();
    let mut buf = Vec::new();
    entry.take(limit + 1).read_to_end(&mut buf).map_err(|e| PackageError::Zip(format!("{name}: {e}")))?;
    if buf.len() as u64 > limit {
        return Err(PackageError::Size { path: name, expected: limit, found: buf.len() as u64 });
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SAMPLE_FILES, key, sample};
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    enum E<'a> {
        File(&'a [u8]),
        Stored(&'a [u8]),
        Dir,
        Symlink(&'a str),
    }

    /// A zip with exactly these entries.
    fn craft(entries: &[(&str, E)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, e) in entries {
            match e {
                E::File(b) => {
                    zip.start_file(*name, opts).unwrap();
                    zip.write_all(b).unwrap();
                }
                E::Stored(b) => {
                    zip.start_file(*name, opts.compression_method(CompressionMethod::Stored)).unwrap();
                    zip.write_all(b).unwrap();
                }
                E::Dir => zip.add_directory(*name, opts).unwrap(),
                E::Symlink(t) => zip.add_symlink(*name, *t, opts).unwrap(),
            }
        }
        zip.finish().unwrap().into_inner()
    }

    /// The sample's entries, with `edit` applied, as a zip.
    fn sample_zip(edit: impl FnOnce(&mut Vec<(String, Vec<u8>)>)) -> Vec<u8> {
        let s = sample("1.0.0", None, None);
        let mut entries: Vec<(String, Vec<u8>)> = vec![("manifest.toml".into(), s.manifest.clone().into_bytes())];
        entries.extend(SAMPLE_FILES.iter().map(|(p, b, _)| (format!("files/{p}"), b.to_vec())));
        edit(&mut entries);
        let refs: Vec<(&str, E)> = entries.iter().map(|(n, b)| (n.as_str(), E::File(b))).collect();
        craft(&refs)
    }

    /// The sample's entries (stored or deflated) plus `extra`, as a zip.
    fn sample_with(manifest: &[u8], stored: bool, extra: Vec<(&str, E)>) -> Vec<u8> {
        let names: Vec<String> = SAMPLE_FILES.iter().map(|(p, _, _)| format!("files/{p}")).collect();
        let entry = |b| if stored { E::Stored(b) } else { E::File(b) };
        let mut entries: Vec<(&str, E)> = vec![("manifest.toml", entry(manifest))];
        for (n, (_, b, _)) in names.iter().zip(SAMPLE_FILES) {
            entries.push((n.as_str(), entry(b)));
        }
        entries.extend(extra);
        craft(&entries)
    }

    fn open(bytes: Vec<u8>) -> Result<Package<Cursor<Vec<u8>>>, PackageError> {
        Package::from_reader(Cursor::new(bytes))
    }

    #[test]
    fn signed_package_round_trip_and_extract() {
        let (publisher, root) = (key(1), key(9));
        let s = sample("1.0.0", Some(&publisher), Some(&root));
        let mut p = open(s.package_bytes()).unwrap();
        assert_eq!(p.manifest().id, "demo");
        assert_eq!(p.manifest_bytes(), s.manifest.as_bytes());
        let signer = p.check_signature(&[PublicKey::of(&root)]).unwrap();
        assert_eq!(signer, Signer::Signed { key: PublicKey::of(&publisher), verified_as: Some("Demo Pub".into()) });
        p.verify_contents().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("cl_plugins/demo/1.0.0");
        assert!(matches!(p.extract(&dest, Platform::LinuxI686), Err(PackageError::Unsupported(_))));
        let files = p.extract(&dest, Platform::LinuxX86_64).unwrap();
        let names: Vec<&str> = files.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "files/bin/demo-host",
                "files/data/readme.txt",
                "files/lib/linux-x86_64/tsmod_demo.so",
                "manifest.sig",
                "manifest.toml",
                "publisher.cert"
            ]
        );
        assert_eq!(files["files/data/readme.txt"], Sha256::of_bytes(b"hello"));
        assert!(!dest.join("files/lib/windows-i686").exists(), "other platforms' files aren't installed");
        assert_eq!(fs::read(dest.join("manifest.toml")).unwrap(), s.manifest.as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(dest.join("files/bin/demo-host")).unwrap().permissions().mode() & 0o111, 0o111);
            assert_eq!(fs::metadata(dest.join("files/data/readme.txt")).unwrap().permissions().mode() & 0o111, 0);
        }
        assert!(matches!(p.extract(&dest, Platform::LinuxX86_64), Err(PackageError::Exists(_))));
        let leftovers: Vec<_> = fs::read_dir(dest.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers, ["1.0.0"]);
    }

    #[test]
    fn entries_must_match_the_manifest() {
        assert!(open(sample_zip(|_| {})).is_ok());

        let extra = sample_zip(|e| e.push(("files/extra.txt".into(), b"surprise".to_vec())));
        assert!(matches!(open(extra), Err(PackageError::Extra(p)) if p == "extra.txt"));

        let missing = sample_zip(|e| e.retain(|(n, _)| n != "files/data/readme.txt"));
        assert!(matches!(open(missing), Err(PackageError::Missing(p)) if p == "data/readme.txt"));

        let no_manifest = sample_zip(|e| e.retain(|(n, _)| n != "manifest.toml"));
        assert!(matches!(open(no_manifest), Err(PackageError::NoManifest)));

        let stray = sample_zip(|e| e.push(("README.md".into(), b"hi".to_vec())));
        assert!(matches!(open(stray), Err(PackageError::UnexpectedEntry(n)) if n == "README.md"));

        let case_dup = sample_zip(|e| e.push(("files/DATA/readme.txt".into(), b"hello".to_vec())));
        assert!(matches!(open(case_dup), Err(PackageError::Duplicate(_))));

        let bad_manifest = sample_zip(|e| e[0].1 = b"format = 1\n".to_vec());
        assert!(matches!(open(bad_manifest), Err(PackageError::Manifest(_))));

        let huge_manifest = sample_zip(|e| e[0].1.extend(std::iter::repeat_n(b'#', MAX_MANIFEST_BYTES as usize)));
        assert!(matches!(open(huge_manifest), Err(PackageError::Manifest(ManifestError::TooLarge(_)))));

        assert!(matches!(open(b"not a zip".to_vec()), Err(PackageError::Zip(_) | PackageError::Io(_))));
    }

    #[test]
    fn path_traversal_and_special_entries_are_refused() {
        let s = sample("1.0.0", None, None);
        let with = |extra: (&str, E)| open(sample_with(s.manifest.as_bytes(), false, vec![extra])).err();
        for name in
            ["files/../../evil.so", "files/a/../../evil", "files/C:/evil.dll", "files/a\\..\\evil", "files/con.txt", "files/x:stream"]
        {
            assert!(matches!(with((name, E::File(b"x"))), Some(PackageError::BadName(..))), "{name}");
        }
        for name in ["../evil", "/etc/evil", "C:/evil", "cl_dlls/tsmod_evil.so", "files"] {
            assert!(matches!(with((name, E::File(b"x"))), Some(PackageError::UnexpectedEntry(_))), "{name}");
        }
        assert!(matches!(with(("files/lib/link.so", E::Symlink("/etc/passwd"))), Some(PackageError::Special(_))));
        assert!(matches!(with(("manifest.sig", E::Symlink("/etc/passwd"))), Some(PackageError::Special(_))));
        assert!(matches!(with(("files/empty/", E::Dir)), Some(PackageError::UnexpectedEntry(_))));
        assert!(matches!(with(("files/../", E::Dir)), Some(PackageError::BadName(..))));
        // Directory entries for listed files' directories are fine (zip -r makes them).
        assert!(with(("files/lib/", E::Dir)).is_none());
        assert!(with(("files/", E::Dir)).is_none());
        // A file and a directory with one name.
        assert!(matches!(with(("files/data/readme.txt/", E::Dir)), Some(PackageError::Duplicate(_))));
    }

    #[test]
    fn duplicate_names_are_refused() {
        // ZipWriter won't write a duplicate name, so write a near-duplicate and patch it (in
        // both the local and the central header).
        let s = sample("1.0.0", None, None);
        let mut bytes = sample_with(s.manifest.as_bytes(), true, vec![("files/data/readme.tx2", E::Stored(b"evil!"))]);
        let from = b"files/data/readme.tx2";
        let mut i = 0;
        while let Some(pos) = bytes[i..].windows(from.len()).position(|w| w == from) {
            bytes[i + pos + from.len() - 1] = b't';
            i += pos + 1;
        }
        assert!(matches!(open(bytes), Err(PackageError::Zip(_) | PackageError::Duplicate(_))));
    }

    #[test]
    fn contents_are_checked_and_failed_extracts_leave_nothing() {
        let replace = |name: &'static str, with: &'static [u8]| {
            sample_zip(move |e| e.iter_mut().find(|(n, _)| n.ends_with(name)).unwrap().1 = with.to_vec())
        };
        // Same length, different bytes: the hash catches it.
        let tampered = replace("readme.txt", b"HELLO");
        assert!(matches!(open(tampered.clone()).unwrap().verify_contents(), Err(PackageError::Hash(p)) if p == "data/readme.txt"));
        let longer = replace("readme.txt", b"hello world");
        assert!(matches!(open(longer).unwrap().verify_contents(), Err(PackageError::Size { expected: 5, found: 6, .. })));

        // Another platform's file is still checked, though it isn't written.
        let other_platform = replace(".dll", b"WINDOWS library");
        let dir = tempfile::tempdir().unwrap();
        for bytes in [tampered, other_platform] {
            let dest = dir.path().join("demo/1.0.0");
            assert!(open(bytes).unwrap().extract(&dest, Platform::LinuxX86_64).is_err());
            assert!(!dest.exists());
            assert_eq!(fs::read_dir(dir.path().join("demo")).unwrap().count(), 0, "no partial directory left");
        }
    }

    #[test]
    fn stored_entries_are_fine() {
        let s = sample("1.0.0", None, None);
        open(sample_with(s.manifest.as_bytes(), true, vec![])).unwrap().verify_contents().unwrap();
    }

    #[test]
    fn builder_refuses_files_that_disagree_with_the_manifest() {
        let s = sample("1.0.0", None, None);
        fs::write(s.src.path().join("data/readme.txt"), b"HELLO").unwrap();
        let r = crate::build::write_package(Cursor::new(Vec::new()), s.manifest.as_bytes(), None, None, s.src.path());
        assert!(matches!(r, Err(PackageError::Hash(_))));
        let e = crate::build::file_entry(s.src.path(), "data/readme.txt", None).unwrap();
        assert_eq!((e.size, e.sha256), (5, Sha256::of_bytes(b"HELLO")));
    }
}
