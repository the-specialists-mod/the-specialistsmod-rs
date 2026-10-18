//! Verification at every start (client-plugins.md §5.2, §5.3).
//!
//! [`discover`] lists the candidates, [`verify`] hashes every file of one and compares the
//! result with the store. Nothing is cached: every file is read in full every time.
//!
//! A plugin loads only on [`Verdict::Load`]. Everything else is a list of [`Change`]s:
//! * changes against the approval (a changed, missing or extra file, the manifest, the version,
//!   the key, the platform, or a plugin seen for the first time) go to the start-up prompt;
//! * problems that make the plugin impossible to approve as it is ([`Change::is_fatal`]: a
//!   broken signature, an invalid manifest, files that disagree with the package's own
//!   manifest, symlinks or special files, unreadable files, no library for this platform) are
//!   reported but get no "Allow". Reinstalling the package is the way out.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use semver::Version;

use crate::hash::{Sha256, hash_files};
use crate::manifest::{MAX_MANIFEST_BYTES, Manifest, is_valid_id};
use crate::package::{CERT_FILE, FILES_DIR, MANIFEST_FILE, SIGNATURE_FILE};
use crate::path::{check_path, check_path_set};
use crate::platform::Platform;
use crate::sign::{MAX_SIGNATURE_FILE_BYTES, PublicKey, Signer, check_signature};
use crate::store::{Entry, MANUAL_PREFIX, STORE_FILE, Scope, State, Store};
use crate::time::now_rfc3339;
use crate::trust::TrustInfo;
use crate::version::VersionChange;

/// `cl_plugins/staging/`: downloads in progress, never candidates.
pub const STAGING_DIR: &str = "staging";
/// The version hand-installed libraries report (§13.2).
pub const MANUAL_VERSION: Version = Version::new(0, 0, 0);

/// Something that might load: an installed package, or a hand-installed library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Candidate {
    /// `cl_plugins/<id>/<version>/`.
    Package {
        /// The version directory.
        dir: PathBuf,
    },
    /// `cl_dlls/tsmod_<name><suffix>`.
    Manual {
        /// `cl_dlls/`.
        dir: PathBuf,
        /// `<name>`.
        name: String,
        /// `<suffix>`.
        suffix: String,
    },
}

impl Candidate {
    /// The id the store knows it by: the directory name, or `manual:<name>`.
    pub fn id(&self) -> String {
        match self {
            Candidate::Package { dir } => {
                dir.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            }
            Candidate::Manual { name, .. } => format!("{MANUAL_PREFIX}{name}"),
        }
    }

    /// The library file name of a hand-installed library: `tsmod_<name><suffix>`.
    fn manual_library(name: &str, suffix: &str) -> String {
        format!("tsmod_{name}{suffix}")
    }
}

/// Lists every candidate: each `cl_plugins/<id>/<version>/` holding a `manifest.toml`, and each
/// `cl_dlls/tsmod_<name><suffix>` (§5.2 step 1). Missing directories have no candidates. The
/// order is stable: packages by id and version, then libraries by name.
pub fn discover(cl_plugins: &Path, cl_dlls: &Path, suffix: &str) -> io::Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for id_dir in sorted_dir(cl_plugins)? {
        let name = id_dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == STAGING_DIR || name.starts_with('.') || !is_real_dir(&id_dir) {
            continue;
        }
        for version_dir in sorted_dir(&id_dir)? {
            let hidden = version_dir.file_name().and_then(|n| n.to_str()).is_none_or(|n| n.starts_with('.'));
            if !hidden && is_real_dir(&version_dir) && fs::symlink_metadata(version_dir.join(MANIFEST_FILE)).is_ok() {
                out.push(Candidate::Package { dir: version_dir });
            }
        }
    }
    for path in sorted_dir(cl_dlls)? {
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(name) = file.strip_prefix("tsmod_").and_then(|n| n.strip_suffix(suffix)) else { continue };
        // On win32 the suffix is plain `.dll`, which a 64-bit `_amd64.dll` also ends with.
        if name.is_empty() || (suffix == ".dll" && name.ends_with("_amd64")) {
            continue;
        }
        out.push(Candidate::Manual { dir: cl_dlls.to_path_buf(), name: name.to_string(), suffix: suffix.to_string() });
    }
    Ok(out)
}

fn sorted_dir(dir: &Path) -> io::Result<Vec<PathBuf>> {
    match fs::read_dir(dir) {
        Ok(rd) => {
            let mut v = rd.map(|e| e.map(|e| e.path())).collect::<io::Result<Vec<_>>>()?;
            v.sort();
            Ok(v)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

fn is_real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir())
}

/// One difference between what's installed and what was approved, or a problem that stops a
/// plugin being approved at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// No approval for this id.
    New,
    /// A file's contents differ from the approval.
    Changed(String),
    /// An approved file is gone.
    Missing(String),
    /// A file the approval doesn't list.
    Extra(String),
    /// `manifest.toml` differs from the approved one.
    Manifest,
    /// Approved at another version.
    Version {
        /// The approved version.
        approved: Version,
        /// The installed one.
        found: Version,
        /// Upgrade, downgrade, rebuild.
        change: VersionChange,
    },
    /// Approved under another publisher key (or signed where it was unsigned, or the reverse):
    /// another plugin under the same id (§3.3).
    Key {
        /// The approved key.
        approved: Option<PublicKey>,
        /// The key now.
        found: Option<PublicKey>,
    },
    /// Approved for another platform.
    Platform {
        /// The approved platform.
        approved: Platform,
        /// This client's.
        found: Platform,
    },
    /// Fatal: the manifest is missing, doesn't parse, or doesn't match its directory.
    Invalid(String),
    /// Fatal: no library for this platform.
    Unsupported(Platform),
    /// Fatal: the signature or countersignature doesn't check.
    Signature(String),
    /// Fatal: installed files disagree with the package's own manifest (or a hand-installed
    /// library's `.files` list names files that aren't there).
    Integrity(Vec<String>),
    /// Fatal: a symlink, device or other non-regular file, or a name that isn't UTF-8.
    Special(String),
    /// Fatal: a file couldn't be read.
    Unreadable(String, String),
}

impl Change {
    /// Whether this stops the plugin being approved as it is.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Change::Invalid(_)
                | Change::Unsupported(_)
                | Change::Signature(_)
                | Change::Integrity(_)
                | Change::Special(_)
                | Change::Unreadable(..)
        )
    }
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Change::New => write!(f, "new plugin"),
            Change::Changed(p) => write!(f, "changed: {p}"),
            Change::Missing(p) => write!(f, "missing: {p}"),
            Change::Extra(p) => write!(f, "extra file: {p}"),
            Change::Manifest => write!(f, "manifest changed"),
            Change::Version { approved, found, change } => {
                write!(f, "version {approved} \u{2192} {found}")?;
                if change.is_downgrade() { write!(f, " (downgrade)") } else { Ok(()) }
            }
            Change::Key { .. } => write!(f, "publisher key changed"),
            Change::Platform { approved, found } => write!(f, "approved for {approved}, this game is {found}"),
            Change::Invalid(why) => write!(f, "invalid: {why}"),
            Change::Unsupported(p) => write!(f, "no library for {p}"),
            Change::Signature(why) => write!(f, "signature: {why}"),
            Change::Integrity(paths) => write!(f, "files don't match the package's manifest: {}", paths.join(", ")),
            Change::Special(p) => write!(f, "not a regular file: {p}"),
            Change::Unreadable(p, e) => write!(f, "can't read {p}: {e}"),
        }
    }
}

/// The prompt's "what changed" lines: up to `max`, then "and N more" (§7.1).
pub fn change_lines(changes: &[Change], max: usize) -> Vec<String> {
    let mut lines: Vec<String> = changes.iter().take(max).map(ToString::to_string).collect();
    if changes.len() > max {
        lines.push(format!("and {} more", changes.len() - max));
    }
    lines
}

/// What to do with a candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every hash matches an allowed entry. (A `scope = "server"` entry still waits for a
    /// listed server: [`Entry::applies_to`].)
    Load,
    /// Blocked ("Never"): don't load, don't prompt.
    Blocked,
    /// Changed or new: don't load; prompt, and load once allowed.
    Prompt,
    /// Can't be approved as it is ([`Change::is_fatal`]): don't load; report.
    Refuse,
}

/// The result of verifying one candidate.
#[derive(Clone, Debug)]
pub struct Verification {
    /// The id: the manifest's, or `manual:<name>`.
    pub id: String,
    /// The version (`0.0.0` for hand-installed libraries; `None` when the manifest is invalid).
    pub version: Option<Version>,
    /// The platform verified for.
    pub platform: Platform,
    /// The directory the file paths are relative to.
    pub base: PathBuf,
    /// The library to load.
    pub library: Option<PathBuf>,
    /// The manifest, when it parsed.
    pub manifest: Option<Manifest>,
    /// SHA-256 of `manifest.toml`.
    pub manifest_sha256: Option<Sha256>,
    /// Who signed it.
    pub signer: Signer,
    /// The trust level to show.
    pub trust: TrustInfo,
    /// Every file found and its hash, relative to `base`. `cl_plugins_verify_log` prints them.
    pub files: BTreeMap<String, Sha256>,
    /// What differs from the approval, and what stops approval.
    pub changes: Vec<Change>,
    /// The decision.
    pub verdict: Verdict,
    /// The allowed entry that matched, on [`Verdict::Load`].
    pub entry: Option<Entry>,
    /// How long hashing took.
    pub elapsed: Duration,
}

impl Verification {
    /// The store entry an "Allow" click writes, or `None` when the plugin can't be approved.
    /// `source` is `server <address>`, `launcher` or `manual`; `servers` are the addresses for
    /// `Scope::Server`.
    pub fn approval(&self, source: &str, scope: Scope, servers: Vec<String>) -> Option<Entry> {
        if self.verdict == Verdict::Refuse {
            return None;
        }
        Some(Entry {
            id: self.id.clone(),
            version: self.version.clone()?,
            state: State::Allowed,
            scope,
            servers,
            source: source.into(),
            publisher_key: self.signer.key(),
            trust: self.trust.trust,
            approved_at: now_rfc3339(),
            manifest_sha256: self.manifest_sha256,
            platform: self.platform,
            files: self.files.clone(),
        })
    }

    /// The store entry a "Never" click writes: blocked everywhere (`servers` empty) or on
    /// `servers`. Pass it to [`Store::block`].
    pub fn blocking(&self, source: &str, servers: Vec<String>) -> Entry {
        Entry {
            id: self.id.clone(),
            version: self.version.clone().unwrap_or(MANUAL_VERSION),
            state: State::Blocked,
            scope: if servers.is_empty() { Scope::Always } else { Scope::Server },
            servers,
            source: source.into(),
            publisher_key: self.signer.key(),
            trust: self.trust.trust,
            approved_at: now_rfc3339(),
            manifest_sha256: self.manifest_sha256,
            platform: self.platform,
            files: BTreeMap::new(),
        }
    }
}

/// Verifies one candidate for `platform` against `store` (§5.2 steps 2–4). `roots` are the
/// countersigning keys ([`crate::builtin_roots`]); `threads` hash in parallel (0: one per
/// core).
pub fn verify(candidate: &Candidate, platform: Platform, store: &Store, roots: &[PublicKey], threads: usize) -> Verification {
    let start = Instant::now();
    let mut v = match candidate {
        Candidate::Package { dir } => scan_package(dir, platform, roots, threads),
        Candidate::Manual { dir, name, suffix } => scan_manual(dir, name, suffix, platform, threads),
    };
    v.trust = TrustInfo::assess(&v.signer, v.manifest.as_ref().and_then(|m| m.publisher.as_ref()).map(|p| p.name.as_str()), store);
    compare(&mut v, store);
    v.elapsed = start.elapsed();
    v
}

fn blank(id: String, platform: Platform, base: &Path) -> Verification {
    Verification {
        id,
        version: None,
        platform,
        base: base.to_path_buf(),
        library: None,
        manifest: None,
        manifest_sha256: None,
        signer: Signer::Unsigned,
        trust: TrustInfo { trust: crate::Trust::Unsigned, publisher: None, fingerprint: None, approved_before: None },
        files: BTreeMap::new(),
        changes: Vec::new(),
        verdict: Verdict::Refuse,
        entry: None,
        elapsed: Duration::ZERO,
    }
}

/// Reads at most `limit` bytes of a regular file; `Ok(None)` when it doesn't exist.
fn read_small(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(m) if !m.file_type().is_file() => return Err("not a regular file".into()),
        Ok(_) => {}
    }
    let mut buf = Vec::new();
    fs::File::open(path).and_then(|f| f.take(limit + 1).read_to_end(&mut buf)).map_err(|e| e.to_string())?;
    if buf.len() as u64 > limit {
        return Err(format!("larger than {limit} bytes"));
    }
    Ok(Some(buf))
}

/// Every file under `root`, as `/`-joined relative paths, and the paths that aren't regular
/// files or directories (or aren't UTF-8). Symlinks are never followed.
pub fn scan_dir(root: &Path) -> io::Result<(Vec<String>, Vec<String>)> {
    let mut files = Vec::new();
    let mut special = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                special.push(format!("{prefix}{}", name.to_string_lossy()));
                continue;
            };
            let rel = format!("{prefix}{name}");
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push((entry.path(), format!("{rel}/")));
            } else if ft.is_file() {
                files.push(rel);
            } else {
                special.push(rel);
            }
        }
    }
    files.sort();
    special.sort();
    Ok((files, special))
}

/// The files a package installed for `platform` must have, and their hashes, relative to its
/// version directory: `manifest.toml` and the signature files as given, and `files/<path>` for
/// each of the manifest's files for `platform`.
pub fn expected_files(manifest: &Manifest, platform: Platform, meta: &[(&str, Sha256)]) -> BTreeMap<String, Sha256> {
    let mut out: BTreeMap<String, Sha256> = meta.iter().map(|(p, h)| (p.to_string(), *h)).collect();
    for f in manifest.files_for(platform) {
        out.insert(format!("{FILES_DIR}/{}", f.path), f.sha256);
    }
    out
}

/// Hashes `rels` under `base` into `v.files`, recording unreadable ones.
fn hash_into(v: &mut Verification, rels: &[String], threads: usize) {
    for (rel, r) in rels.iter().zip(hash_files(&v.base, rels, threads)) {
        match r {
            Ok((h, _)) => {
                v.files.insert(rel.clone(), h);
            }
            Err(e) => v.changes.push(Change::Unreadable(rel.clone(), e.to_string())),
        }
    }
}

fn scan_package(dir: &Path, platform: Platform, roots: &[PublicKey], threads: usize) -> Verification {
    let dir_name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut v = blank(Candidate::Package { dir: dir.to_path_buf() }.id(), platform, dir);
    let bytes = match read_small(&dir.join(MANIFEST_FILE), MAX_MANIFEST_BYTES) {
        Ok(Some(b)) => b,
        Ok(None) => return invalid(v, "no manifest.toml".into()),
        Err(e) => return invalid(v, format!("manifest.toml: {e}")),
    };
    let manifest = match Manifest::parse(&bytes) {
        Ok(m) => m,
        Err(e) => return invalid(v, e.to_string()),
    };
    let manifest_sha = Sha256::of_bytes(&bytes);
    v.manifest_sha256 = Some(manifest_sha);
    v.version = Some(manifest.version.clone());
    if manifest.id != v.id || manifest.version.to_string() != dir_name(dir) {
        let why = format!("manifest says {}/{}, installed as {}/{}", manifest.id, manifest.version, v.id, dir_name(dir));
        v.manifest = Some(manifest);
        return invalid(v, why);
    }
    v.id = manifest.id.clone();
    match manifest.library(platform) {
        Some(lib) => v.library = Some(dir.join(FILES_DIR).join(lib)),
        None => v.changes.push(Change::Unsupported(platform)),
    }

    let mut meta = vec![(MANIFEST_FILE, manifest_sha)];
    let mut sig_files: [Option<Vec<u8>>; 2] = [None, None];
    for (slot, name) in sig_files.iter_mut().zip([SIGNATURE_FILE, CERT_FILE]) {
        match read_small(&dir.join(name), MAX_SIGNATURE_FILE_BYTES) {
            Ok(Some(b)) => {
                meta.push((name, Sha256::of_bytes(&b)));
                *slot = Some(b);
            }
            Ok(None) => {}
            Err(e) => v.changes.push(Change::Signature(format!("{name}: {e}"))),
        }
    }
    match check_signature(&manifest, &bytes, sig_files[0].as_deref(), sig_files[1].as_deref(), roots) {
        Ok(s) => v.signer = s,
        Err(e) => v.changes.push(Change::Signature(e.to_string())),
    }

    let (found, special) = match scan_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            v.manifest = Some(manifest);
            return invalid(v, format!("can't list the package directory: {e}"));
        }
    };
    v.changes.extend(special.into_iter().map(Change::Special));
    hash_into(&mut v, &found, threads);

    let expected = expected_files(&manifest, platform, &meta);
    let mut mismatched: Vec<String> = expected.iter().filter(|(p, h)| v.files.get(*p) != Some(h)).map(|(p, _)| p.clone()).collect();
    mismatched.extend(v.files.keys().filter(|p| !expected.contains_key(*p)).cloned());
    // Unreadable files already have their own change.
    mismatched.retain(|p| !v.changes.iter().any(|c| matches!(c, Change::Unreadable(u, _) if u == p)));
    if !mismatched.is_empty() {
        mismatched.sort();
        v.changes.push(Change::Integrity(mismatched));
    }
    v.manifest = Some(manifest);
    v
}

fn invalid(mut v: Verification, why: String) -> Verification {
    v.changes.push(Change::Invalid(why));
    v
}

fn scan_manual(dir: &Path, name: &str, suffix: &str, platform: Platform, threads: usize) -> Verification {
    let mut v = blank(format!("{MANUAL_PREFIX}{name}"), platform, dir);
    v.version = Some(MANUAL_VERSION);
    let library = Candidate::manual_library(name, suffix);
    v.library = Some(dir.join(&library));
    if !is_valid_id(name) {
        return invalid(v, format!("{library}: <name> must be [a-z0-9_-]{{1,32}}"));
    }
    let list_name = format!("tsmod_{name}.files");
    let mut rels = vec![library.clone()];
    match read_small(&dir.join(&list_name), MAX_MANIFEST_BYTES) {
        Ok(None) => {}
        Ok(Some(bytes)) => {
            let Ok(text) = String::from_utf8(bytes) else { return invalid(v, format!("{list_name} isn't UTF-8")) };
            rels.push(list_name.clone());
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                if let Err(e) = check_path(line) {
                    return invalid(v, format!("{list_name}: {e}"));
                }
                rels.push(line.to_string());
            }
            if let Err(e) = check_path_set(rels.iter().map(String::as_str)) {
                return invalid(v, format!("{list_name}: {e}"));
            }
        }
        Err(e) => return invalid(v, format!("{list_name}: {e}")),
    }
    let mut missing = Vec::new();
    let mut present = Vec::new();
    for rel in rels {
        match fs::symlink_metadata(dir.join(&rel)) {
            Ok(m) if m.file_type().is_file() => present.push(rel),
            Ok(_) => v.changes.push(Change::Special(rel)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => missing.push(rel),
            Err(e) => v.changes.push(Change::Unreadable(rel, e.to_string())),
        }
    }
    hash_into(&mut v, &present, threads);
    if !missing.is_empty() {
        v.changes.push(Change::Integrity(missing));
    }
    v
}

/// Compares the hashed files with the store and sets the verdict (§5.2 step 3).
fn compare(v: &mut Verification, store: &Store) {
    // A signature that fails is reported as such; compared under the key the manifest claims,
    // it doesn't also show up as a key change.
    let signature_failed = v.changes.iter().any(|c| matches!(c, Change::Signature(_)));
    let claimed = || v.manifest.as_ref().and_then(|m| m.publisher.as_ref()).and_then(|p| p.key);
    let key = if signature_failed { claimed() } else { v.signer.key() };
    let fatal = v.changes.iter().any(Change::is_fatal);
    if store.is_blocked(&v.id, key, None) {
        v.verdict = Verdict::Blocked;
        return;
    }
    // Without a usable manifest or file list there's nothing meaningful to compare.
    let invalid = v.changes.iter().any(|c| matches!(c, Change::Invalid(_)));
    let Some(version) = v.version.clone().filter(|_| !invalid) else {
        v.verdict = Verdict::Refuse;
        return;
    };
    let allowed: Vec<&Entry> = store.entries_for(&v.id).filter(|e| e.state == State::Allowed).collect();
    let exact = allowed.iter().find(|e| e.version == version && e.platform == v.platform);
    let mut changes = Vec::new();
    match exact {
        Some(e) if e.publisher_key != key => changes.push(Change::Key { approved: e.publisher_key, found: key }),
        Some(e) => {
            let manifest_changed = e.manifest_sha256 != v.manifest_sha256;
            if manifest_changed {
                changes.push(Change::Manifest);
            }
            changes.extend(
                diff_files(&e.files, &v.files).into_iter().filter(|c| !(manifest_changed && *c == Change::Changed(MANIFEST_FILE.into()))),
            );
        }
        None if allowed.is_empty() => changes.push(Change::New),
        None => {
            let same_key: Vec<&&Entry> = allowed.iter().filter(|e| e.publisher_key == key).collect();
            if same_key.is_empty() {
                changes.push(Change::Key { approved: allowed[0].publisher_key, found: key });
            } else if let Some(e) = same_key.iter().find(|e| e.version == version) {
                changes.push(Change::Platform { approved: e.platform, found: v.platform });
            } else {
                let e = same_key.iter().find(|e| e.platform == v.platform).unwrap_or(&same_key[0]);
                let change = VersionChange::between(&e.version, &version);
                changes.push(Change::Version { approved: e.version.clone(), found: version, change });
            }
        }
    }
    v.verdict = if fatal {
        Verdict::Refuse
    } else if changes.is_empty() {
        v.entry = exact.map(|e| (*e).clone());
        Verdict::Load
    } else {
        Verdict::Prompt
    };
    // Approval differences first: they're what the prompt is about.
    changes.append(&mut v.changes);
    v.changes = changes;
}

/// The file differences between an approval and what was found.
pub fn diff_files(approved: &BTreeMap<String, Sha256>, found: &BTreeMap<String, Sha256>) -> Vec<Change> {
    let paths: BTreeSet<&String> = approved.keys().chain(found.keys()).collect();
    paths
        .into_iter()
        .filter_map(|p| match (approved.get(p), found.get(p)) {
            (Some(a), Some(f)) if a != f => Some(Change::Changed(p.clone())),
            (Some(_), None) => Some(Change::Missing(p.clone())),
            (None, Some(_)) => Some(Change::Extra(p.clone())),
            _ => None,
        })
        .collect()
}

/// [`verify`] for every candidate [`discover`] finds, in its order. Also reports why the store
/// path isn't usable.
pub fn verify_all(
    cl_plugins: &Path,
    cl_dlls: &Path,
    suffix: &str,
    platform: Platform,
    store: &Store,
    roots: &[PublicKey],
    threads: usize,
) -> io::Result<Vec<Verification>> {
    Ok(discover(cl_plugins, cl_dlls, suffix)?.iter().map(|c| verify(c, platform, store, roots, threads)).collect())
}

/// The store's path in `cl_plugins/`.
pub fn store_path(cl_plugins: &Path) -> PathBuf {
    cl_plugins.join(STORE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::Package;
    use crate::sign::sign_manifest;
    use crate::test_util::{Sample, key, sample};
    use crate::trust::Trust;
    use ed25519_dalek::SigningKey;

    const P: Platform = Platform::LinuxX86_64;
    const README: &str = "files/data/readme.txt";

    struct Env {
        root: tempfile::TempDir,
    }

    impl Env {
        fn new() -> Env {
            Env { root: tempfile::tempdir().unwrap() }
        }
        fn cl_plugins(&self) -> PathBuf {
            self.root.path().join("cl_plugins")
        }
        fn cl_dlls(&self) -> PathBuf {
            self.root.path().join("cl_dlls")
        }
        /// Installs `s` as cl_plugins/demo/<version>/ (replacing an older install).
        fn install(&self, s: &Sample, version: &str) -> PathBuf {
            let pkg = self.root.path().join("demo.tspkg");
            s.write_package(&pkg);
            let dest = self.cl_plugins().join("demo").join(version);
            let _ = fs::remove_dir_all(&dest);
            Package::open(&pkg).unwrap().extract(&dest, P).unwrap();
            dest
        }
    }

    fn check(dir: &Path, store: &Store) -> Verification {
        verify(&Candidate::Package { dir: dir.to_path_buf() }, P, store, &[], 2)
    }

    /// Verifies, approves and re-verifies: the approved state the tests start from.
    fn approved(env: &Env, publisher: Option<&SigningKey>) -> (PathBuf, Store) {
        let dir = env.install(&sample("1.0.0", publisher, None), "1.0.0");
        let mut store = Store::new();
        let v = check(&dir, &store);
        assert_eq!((v.verdict, v.changes.as_slice()), (Verdict::Prompt, &[Change::New][..]), "{:?}", v.changes);
        store.allow(v.approval("launcher", Scope::Always, vec![]).unwrap());
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Load, "{:?}", v.changes);
        assert_eq!(v.library.as_deref(), Some(dir.join("files/lib/linux-x86_64/tsmod_demo.so").as_path()));
        (dir, store)
    }

    fn has(v: &Verification, c: &Change) -> bool {
        v.changes.contains(c)
    }

    #[test]
    fn approval_round_trip_through_the_saved_store() {
        let env = Env::new();
        let (dir, store) = approved(&env, Some(&key(1)));
        let path = store_path(&env.cl_plugins());
        store.save(&path).unwrap();
        let (loaded, _) = Store::load(&path).unwrap();
        let v = check(&dir, &loaded);
        assert_eq!(v.verdict, Verdict::Load);
        let e = v.entry.unwrap();
        assert_eq!(e.publisher_key, Some(PublicKey::of(&key(1))));
        assert_eq!(e.trust, Trust::New);
        assert_eq!(e.files.len(), 5, "manifest.toml, manifest.sig and the three linux files");
        assert_eq!(e.files.get(README), Some(&Sha256::of_bytes(b"hello")));
    }

    #[test]
    fn changed_missing_and_extra_files_stop_loading() {
        let env = Env::new();
        let (dir, store) = approved(&env, None);

        // One changed byte, with the size and mtime put back: only a full re-hash sees it.
        let path = dir.join(README);
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, b"jello").unwrap();
        fs::File::options().write(true).open(&path).unwrap().set_modified(mtime).unwrap();
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Refuse);
        assert!(has(&v, &Change::Changed(README.into())), "{:?}", v.changes);
        assert!(has(&v, &Change::Integrity(vec![README.into()])));
        assert!(v.approval("launcher", Scope::Always, vec![]).is_none(), "can't approve files that contradict the manifest");
        fs::write(&path, b"hello").unwrap();
        assert_eq!(check(&dir, &store).verdict, Verdict::Load);

        fs::remove_file(&path).unwrap();
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Refuse);
        assert!(has(&v, &Change::Missing(README.into())));
        fs::write(&path, b"hello").unwrap();

        for extra in ["files/data/extra.txt", "files/lib/linux-x86_64/libevil.so", "notes.txt"] {
            let p = dir.join(extra);
            fs::write(&p, b"x").unwrap();
            let v = check(&dir, &store);
            assert_eq!(v.verdict, Verdict::Refuse, "{extra}");
            assert!(has(&v, &Change::Extra(extra.into())), "{extra}: {:?}", v.changes);
            fs::remove_file(&p).unwrap();
        }
        // An extra empty directory isn't a file.
        fs::create_dir(dir.join("files/cache")).unwrap();
        assert_eq!(check(&dir, &store).verdict, Verdict::Load);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_in_a_package_directory_are_refused() {
        let env = Env::new();
        let (dir, store) = approved(&env, None);
        std::os::unix::fs::symlink("/etc/passwd", dir.join("files/data/link")).unwrap();
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Refuse);
        assert!(has(&v, &Change::Special("files/data/link".into())));
        fs::remove_file(dir.join("files/data/link")).unwrap();
        // A file replaced by a symlink to identical bytes is still refused.
        let real = env.root.path().join("readme-copy");
        fs::write(&real, b"hello").unwrap();
        fs::remove_file(dir.join(README)).unwrap();
        std::os::unix::fs::symlink(&real, dir.join(README)).unwrap();
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Refuse);
        assert!(has(&v, &Change::Special(README.into())));
    }

    #[test]
    fn manifest_change_prompts_again() {
        let env = Env::new();
        let k = key(1);
        let (_, store) = approved(&env, Some(&k));
        // A rebuilt 1.0.0 with a new description, consistent with its own new signature.
        let mut s = sample("1.0.0", Some(&k), None);
        s.manifest = s.manifest.replacen("client_ext_api = 1", "client_ext_api = 1\ndescription = \"now with more\"", 1);
        s.sig = Some(sign_manifest(&k, s.manifest.as_bytes()));
        let dir = env.install(&s, "1.0.0");
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Prompt);
        assert_eq!(v.changes, [Change::Manifest, Change::Changed("manifest.sig".into())]);
        let mut store = store;
        store.allow(v.approval("launcher", Scope::Always, vec![]).unwrap());
        assert_eq!(check(&dir, &store).verdict, Verdict::Load);
    }

    #[test]
    fn version_changes_and_downgrades() {
        let env = Env::new();
        let (old_dir, store) = approved(&env, None);
        fs::remove_dir_all(&old_dir).unwrap();
        let dir = env.install(&sample("1.1.0", None, None), "1.1.0");
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Prompt);
        let up = Change::Version { approved: "1.0.0".parse().unwrap(), found: "1.1.0".parse().unwrap(), change: VersionChange::Upgrade };
        assert_eq!(v.changes, [up]);

        let mut store = store;
        store.allow(v.approval("launcher", Scope::Always, vec![]).unwrap());
        fs::remove_dir_all(&dir).unwrap();
        let dir = env.install(&sample("1.0.0", None, None), "1.0.0");
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Prompt);
        assert!(matches!(&v.changes[0], Change::Version { change: VersionChange::Downgrade, .. }));
        assert_eq!(v.changes[0].to_string(), "version 1.1.0 \u{2192} 1.0.0 (downgrade)");
        assert_eq!(store.entries_for("demo").count(), 1, "approving 1.1.0 replaced 1.0.0's entry");
    }

    #[test]
    fn key_changes_are_another_plugin() {
        let env = Env::new();
        let (_, store) = approved(&env, Some(&key(1)));
        let dir = env.install(&sample("1.0.0", Some(&key(2)), None), "1.0.0");
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Prompt);
        assert_eq!(v.changes, [Change::Key { approved: Some(PublicKey::of(&key(1))), found: Some(PublicKey::of(&key(2))) }]);
        assert_eq!(v.trust.trust, Trust::New);
        // Unsigned now, where it was signed.
        let dir = env.install(&sample("1.0.0", None, None), "1.0.0");
        assert_eq!(check(&dir, &store).changes, [Change::Key { approved: Some(PublicKey::of(&key(1))), found: None }]);
        // A newer version under another key: still a key change, not a version change.
        let dir = env.install(&sample("2.0.0", Some(&key(2)), None), "2.0.0");
        assert!(matches!(check(&dir, &store).changes[..], [Change::Key { .. }]));
    }

    #[test]
    fn platform_change() {
        let env = Env::new();
        let (dir, mut store) = approved(&env, None);
        store.plugins[0].platform = Platform::WindowsI686;
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Prompt);
        assert_eq!(v.changes, [Change::Platform { approved: Platform::WindowsI686, found: P }]);
        // A platform the package has no library for can't be approved.
        let v = verify(&Candidate::Package { dir: dir.clone() }, Platform::LinuxI686, &Store::new(), &[], 1);
        assert_eq!(v.verdict, Verdict::Refuse);
        assert!(has(&v, &Change::Unsupported(Platform::LinuxI686)));
    }

    #[test]
    fn broken_signatures_are_refused() {
        let env = Env::new();
        let (dir, store) = approved(&env, Some(&key(1)));
        let mut sig = fs::read(dir.join("manifest.sig")).unwrap();
        sig[0] ^= 1;
        fs::write(dir.join("manifest.sig"), &sig).unwrap();
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Refuse);
        assert!(v.changes.iter().any(|c| matches!(c, Change::Signature(_))), "{:?}", v.changes);
        assert!(has(&v, &Change::Changed("manifest.sig".into())));
        // Deleting the signature makes it unsigned: a key change, and the manifest still
        // matches, so it's a prompt, not a load.
        fs::remove_file(dir.join("manifest.sig")).unwrap();
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Prompt);
        assert_eq!(v.changes, [Change::Key { approved: Some(PublicKey::of(&key(1))), found: None }]);
    }

    #[test]
    fn verified_and_known_publishers() {
        let env = Env::new();
        let (publisher, root) = (key(1), key(9));
        let dir = env.install(&sample("1.0.0", Some(&publisher), Some(&root)), "1.0.0");
        let roots = [PublicKey::of(&root)];
        let v = verify(&Candidate::Package { dir: dir.clone() }, P, &Store::new(), &roots, 1);
        assert_eq!(v.trust.trust, Trust::Verified);
        // Without the root built in, the same package is a new publisher; once one of its
        // plugins is approved, a known one.
        let mut store = Store::new();
        let v = check(&dir, &store);
        assert_eq!(v.trust.trust, Trust::New);
        let mut e = v.approval("launcher", Scope::Always, vec![]).unwrap();
        e.id = "other".into();
        store.allow(e);
        assert_eq!(check(&dir, &store).trust.trust, Trust::Known);
    }

    #[test]
    fn blocked_and_server_scoped() {
        let env = Env::new();
        let (dir, mut store) = approved(&env, None);
        store.plugins[0].scope = Scope::Server;
        store.plugins[0].servers = vec!["203.0.113.5:27015".into()];
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Load);
        let e = v.entry.as_ref().unwrap();
        assert!(e.applies_to(Some("203.0.113.5:27015")) && !e.applies_to(None) && !e.applies_to(Some("10.0.0.1:27015")));

        store.block(v.blocking("manual", vec![]));
        let v = check(&dir, &store);
        assert_eq!(v.verdict, Verdict::Blocked);
        // A block is per key: the same id from a signed publisher prompts.
        let signed = env.install(&sample("1.0.0", Some(&key(3)), None), "1.0.0");
        assert_eq!(check(&signed, &store).verdict, Verdict::Prompt);
    }

    #[test]
    fn invalid_install_directories() {
        let env = Env::new();
        let (dir, store) = approved(&env, None);
        let moved = env.cl_plugins().join("demo/9.9.9");
        fs::rename(&dir, &moved).unwrap();
        let v = check(&moved, &store);
        assert_eq!(v.verdict, Verdict::Refuse);
        assert!(matches!(&v.changes[0], Change::Invalid(_)));
        let other_id = env.cl_plugins().join("evil/1.0.0");
        fs::create_dir_all(other_id.parent().unwrap()).unwrap();
        fs::rename(&moved, &other_id).unwrap();
        assert_eq!(check(&other_id, &store).verdict, Verdict::Refuse);
        fs::write(other_id.join("manifest.toml"), b"format = 1\n").unwrap();
        assert!(matches!(&check(&other_id, &store).changes[..], [Change::Invalid(_)]));
        assert!(matches!(&check(&moved_back(&env, &other_id), &store).changes[..], [Change::Invalid(_)]));
    }

    fn moved_back(env: &Env, dir: &Path) -> PathBuf {
        let back = env.cl_plugins().join("demo/1.0.0");
        fs::rename(dir, &back).unwrap();
        back
    }

    #[test]
    fn hand_installed_libraries() {
        let env = Env::new();
        let dlls = env.cl_dlls();
        fs::create_dir_all(dlls.join("dev")).unwrap();
        fs::write(dlls.join("tsmod_dev_amd64.so"), b"build 1").unwrap();
        let c = Candidate::Manual { dir: dlls.clone(), name: "dev".into(), suffix: "_amd64.so".into() };
        let mut store = Store::new();
        let v = verify(&c, P, &store, &[], 1);
        assert_eq!((v.id.as_str(), v.verdict), ("manual:dev", Verdict::Prompt));
        assert_eq!(v.changes, [Change::New]);
        assert_eq!(v.trust.trust, Trust::Unsigned);
        assert_eq!(v.version, Some(MANUAL_VERSION));
        store.allow(v.approval("manual", Scope::Always, vec![]).unwrap());
        assert_eq!(verify(&c, P, &store, &[], 1).verdict, Verdict::Load);

        // Every rebuild prompts again.
        fs::write(dlls.join("tsmod_dev_amd64.so"), b"build 2").unwrap();
        let v = verify(&c, P, &store, &[], 1);
        assert_eq!((v.verdict, v.changes.as_slice()), (Verdict::Prompt, &[Change::Changed("tsmod_dev_amd64.so".into())][..]));
        store.allow(v.approval("manual", Scope::Always, vec![]).unwrap());

        // A .files list adds files; adding it is a change.
        fs::write(dlls.join("dev/data.bin"), b"data").unwrap();
        fs::write(dlls.join("tsmod_dev.files"), "# extra files\ndev/data.bin\n").unwrap();
        let v = verify(&c, P, &store, &[], 1);
        assert_eq!(v.verdict, Verdict::Prompt);
        assert_eq!(v.changes, [Change::Extra("dev/data.bin".into()), Change::Extra("tsmod_dev.files".into())]);
        store.allow(v.approval("manual", Scope::Always, vec![]).unwrap());
        fs::write(dlls.join("dev/data.bin"), b"DATA").unwrap();
        assert_eq!(verify(&c, P, &store, &[], 1).changes, [Change::Changed("dev/data.bin".into())]);

        // A listed file that's gone can't be approved; a listed path outside cl_dlls is invalid.
        fs::remove_file(dlls.join("dev/data.bin")).unwrap();
        let v = verify(&c, P, &store, &[], 1);
        assert_eq!(v.verdict, Verdict::Refuse);
        assert!(has(&v, &Change::Missing("dev/data.bin".into())));
        fs::write(dlls.join("tsmod_dev.files"), "../../etc/passwd\n").unwrap();
        assert!(matches!(&verify(&c, P, &store, &[], 1).changes[..], [Change::Invalid(_)]));

        let bad = Candidate::Manual { dir: dlls.clone(), name: "Dev".into(), suffix: "_amd64.so".into() };
        assert_eq!(verify(&bad, P, &store, &[], 1).verdict, Verdict::Refuse);
    }

    #[test]
    fn discovery() {
        let env = Env::new();
        assert_eq!(discover(&env.cl_plugins(), &env.cl_dlls(), "_amd64.so").unwrap(), []);
        let (dir, _) = approved(&env, None);
        let cl = env.cl_plugins();
        fs::create_dir_all(cl.join("staging/demo/1.0.0")).unwrap();
        fs::write(cl.join("staging/demo/1.0.0/manifest.toml"), b"").unwrap();
        fs::create_dir_all(cl.join("demo/.1.1.0.partial")).unwrap();
        fs::write(cl.join("demo/.1.1.0.partial/manifest.toml"), b"").unwrap();
        fs::create_dir_all(cl.join("empty/1.0.0")).unwrap();
        fs::write(cl.join(STORE_FILE), b"").unwrap();
        let dlls = env.cl_dlls();
        fs::create_dir_all(&dlls).unwrap();
        for f in ["client.so", "tsmod_a_amd64.so", "tsmod_b_i386.so", "tsmod__amd64.so", "tsmod_c_amd64.so.bak"] {
            fs::write(dlls.join(f), b"x").unwrap();
        }
        let found = discover(&cl, &dlls, "_amd64.so").unwrap();
        assert_eq!(
            found,
            [
                Candidate::Package { dir: dir.clone() },
                Candidate::Manual { dir: dlls.clone(), name: "a".into(), suffix: "_amd64.so".into() }
            ]
        );
        assert_eq!(found[0].id(), "demo");
        fs::write(dlls.join("tsmod_w.dll"), b"x").unwrap();
        fs::write(dlls.join("tsmod_w_amd64.dll"), b"x").unwrap();
        let win32 = discover(&cl, &dlls, ".dll").unwrap();
        assert_eq!(win32.iter().map(Candidate::id).collect::<Vec<_>>(), ["demo", "manual:w"]);

        let store = Store::new();
        let all = verify_all(&cl, &dlls, "_amd64.so", P, &store, &[], 0).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|v| v.verdict == Verdict::Prompt));
    }

    #[test]
    fn change_lines_cap() {
        let changes: Vec<Change> = (0..13).map(|i| Change::Changed(format!("f{i}"))).collect();
        let lines = change_lines(&changes, 10);
        assert_eq!(lines.len(), 11);
        assert_eq!(lines[0], "changed: f0");
        assert_eq!(lines[10], "and 3 more");
        assert_eq!(change_lines(&changes[..2], 10).len(), 2);
    }
}
