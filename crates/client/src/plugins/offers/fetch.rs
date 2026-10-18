//! The offer flow's worker threads (client-plugins.md §6.3 steps 2, 4–6): HTTP(S) with `ureq` and
//! `rustls`, then the package's checks and its install, all off the game's thread. Each job runs
//! on a thread of its own and sends one [`Done`] back; the game polls them once a frame, so a slow
//! server or a large package never holds a frame up.
//!
//! * HTTPS only, redirects included, unless the offer is FastDL's (`plain_http`); at most 3
//!   redirects; a status other than 2xx is an error (a missing `.manifest.sig` or
//!   `.publisher.cert` is fine: the package is then unsigned or not countersigned).
//! * The manifest and its siblings are read up to their size limits ([`plugin_pkg::manifest::MAX_MANIFEST_BYTES`],
//!   [`plugin_pkg::sign::MAX_SIGNATURE_FILE_BYTES`]); the package up to the offer's size, and the
//!   download stops as soon as it goes past it.
//! * Cancelling (disconnect, `cl_plugins_cancel`, a withdrawn offer) stops the download between
//!   reads; the staging file is removed whatever happens.

use super::flow::{CERT_SUFFIX, Fetched, MANIFEST_SUFFIX, SIGNATURE_SUFFIX, Transport};
use plugin_pkg::{Candidate, Package, Platform, PublicKey, Sha256, Store, Verdict, Verification};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

/// A worker's result, tagged with the job's key.
#[derive(Debug)]
pub enum Done {
    Manifest { key: u32, result: Result<Fetched, String> },
    Install { key: u32, result: Result<Box<Verification>, String> },
}

/// Shared with the game: cancel, and the bytes downloaded so far.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub cancel: Arc<AtomicBool>,
    pub bytes: Arc<AtomicU64>,
}

impl Progress {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

fn agent(plain_http: bool, body: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(!plain_http)
        .max_redirects(3)
        .http_status_as_error(true)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(body))
        .user_agent(format!("the-specialists-rs/{}", shared::protocol::port::VERSION))
        .build()
        .into()
}

/// Reads `url` whole, up to `limit` bytes. `Ok(None)` for a 404 when `optional`.
fn get_small(agent: &ureq::Agent, url: &str, limit: u64, optional: bool) -> Result<Option<Vec<u8>>, String> {
    let resp = match agent.get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(404)) if optional => return Ok(None),
        Err(e) => return Err(format!("{url}: {e}")),
    };
    let mut buf = Vec::new();
    resp.into_body().into_reader().take(limit + 1).read_to_end(&mut buf).map_err(|e| format!("{url}: {e}"))?;
    if buf.len() as u64 > limit {
        return Err(format!("{url} is over {limit} bytes"));
    }
    Ok(Some(buf))
}

/// Step 2: the manifest and its signature and countersignature.
pub fn fetch_manifest(t: &Transport) -> Result<Fetched, String> {
    let a = agent(t.plain_http, Duration::from_secs(30));
    let manifest =
        get_small(&a, &format!("{}{MANIFEST_SUFFIX}", t.base), plugin_pkg::manifest::MAX_MANIFEST_BYTES, false)?.unwrap_or_default();
    let sig_limit = plugin_pkg::sign::MAX_SIGNATURE_FILE_BYTES;
    let signature = get_small(&a, &format!("{}{SIGNATURE_SUFFIX}", t.base), sig_limit, true)?;
    let cert = if signature.is_some() { get_small(&a, &format!("{}{CERT_SUFFIX}", t.base), sig_limit, true)? } else { None };
    Ok(Fetched { manifest, signature, cert })
}

/// What [`install`] needs: the offer, what the card showed, and where things go.
#[derive(Clone, Debug)]
pub struct InstallJob {
    pub url: String,
    pub plain_http: bool,
    pub size: u32,
    pub sha256: [u8; 32],
    /// The manifest the card showed: the package's must be byte-identical (§6.3 step 5).
    pub manifest_sha256: Sha256,
    /// The key the card's manifest verified with.
    pub key: Option<PublicKey>,
    /// `cl_plugins/staging/<id>-<version>.tspkg.part`.
    pub staging: PathBuf,
    /// `cl_plugins/<id>/<version>/`.
    pub dest: PathBuf,
    /// The installed copy to reuse instead of downloading (the same manifest, approved for
    /// other servers).
    pub reuse: bool,
    pub platform: Platform,
    pub roots: Vec<PublicKey>,
    /// The store as it was, for the trust level the approval records.
    pub store: Store,
}

/// Steps 4–6: download (or reuse), check, unpack, and verify the install. On success the
/// installed plugin's verification, from which the approval is written.
pub fn install(j: &InstallJob, p: &Progress) -> Result<Verification, String> {
    if !j.reuse {
        let r = download(j, p).and_then(|()| unpack(j));
        let _ = fs::remove_file(&j.staging);
        r?;
    }
    let v = plugin_pkg::verify(&Candidate::Package { dir: j.dest.clone() }, j.platform, &j.store, &j.roots, 0);
    let fail = |why: String| {
        if !j.reuse {
            let _ = fs::remove_dir_all(&j.dest);
        }
        Err(why)
    };
    if v.verdict == Verdict::Refuse {
        return fail(format!("the installed files don't verify: {}", plugin_pkg::change_lines(&v.changes, 3).join("; ")));
    }
    if v.manifest_sha256 != Some(j.manifest_sha256) {
        return fail("the installed manifest isn't the one approved".into());
    }
    if v.signer.key() != j.key {
        return fail("the installed package's signer isn't the one approved".into());
    }
    Ok(v)
}

fn download(j: &InstallJob, p: &Progress) -> Result<(), String> {
    let size = u64::from(j.size);
    // At least 64 KiB/s, and a minute for small packages.
    let body = Duration::from_secs(60 + size / (64 * 1024));
    let resp = agent(j.plain_http, body).get(&j.url).call().map_err(|e| format!("download: {e}"))?;
    let body = resp.into_body();
    if body.content_length().is_some_and(|n| n != size) {
        return Err(format!("the server sends {} bytes, the offer says {size}", body.content_length().unwrap_or(0)));
    }
    if let Some(dir) = j.staging.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut out = fs::File::create(&j.staging).map_err(|e| format!("{}: {e}", j.staging.display()))?;
    let mut reader = body.into_reader();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        if p.cancelled() {
            return Err("cancelled".into());
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("download: {e}")),
        };
        total += n as u64;
        if total > size {
            return Err(format!("the download is bigger than the offer's {size} bytes"));
        }
        out.write_all(&buf[..n]).map_err(|e| format!("{}: {e}", j.staging.display()))?;
        p.bytes.store(total, Ordering::Relaxed);
    }
    if total != size {
        return Err(format!("the download is {total} bytes, the offer says {size}"));
    }
    out.sync_all().map_err(|e| e.to_string())?;
    drop(out);
    let (sha, _) = Sha256::of_file(&j.staging).map_err(|e| format!("{}: {e}", j.staging.display()))?;
    if sha.0 != j.sha256 {
        return Err("the package's SHA-256 isn't the offer's".into());
    }
    Ok(())
}

/// Step 5 and 6's unpacking: the package's manifest is the card's, its signature still checks
/// under the same key, and every entry is its manifest's ([`Package::extract`] checks each
/// file's size and hash, unpacks beside the destination and renames it into place).
fn unpack(j: &InstallJob) -> Result<(), String> {
    let mut pkg = Package::open(&j.staging).map_err(|e| format!("package: {e}"))?;
    if pkg.manifest_sha256() != j.manifest_sha256 {
        return Err("the package's manifest isn't the one shown".into());
    }
    let signer = pkg.check_signature(&j.roots).map_err(|e| format!("package signature: {e}"))?;
    if signer.key() != j.key {
        return Err("the package's signer isn't the one shown".into());
    }
    set_aside(&j.dest)?;
    pkg.extract(&j.dest, j.platform).map_err(|e| format!("unpack: {e}"))?;
    Ok(())
}

/// Moves an existing (unapproved, or replaced) version directory out of the way: renamed to a
/// hidden `.<version>.replaced`, which is never a candidate and goes at the next start, or
/// removed when it can't be renamed.
fn set_aside(dest: &Path) -> Result<(), String> {
    if fs::symlink_metadata(dest).is_err() {
        return Ok(());
    }
    let leaf = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let aside = dest.with_file_name(format!(".{leaf}.replaced"));
    let _ = fs::remove_dir_all(&aside);
    if fs::rename(dest, &aside).is_ok() {
        return Ok(());
    }
    fs::remove_dir_all(dest).map_err(|e| format!("{} is in the way: {e}", dest.display()))
}

/// Runs `f` on a thread of its own and sends its result.
pub fn spawn(tx: &Sender<Done>, f: impl FnOnce() -> Done + Send + 'static) {
    let tx = tx.clone();
    let _ = std::thread::Builder::new().name("tsrs-plugin-offer".into()).spawn(move || {
        let _ = tx.send(f());
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::BufRead;
    use std::net::TcpListener;

    /// A tiny HTTP/1.1 server on 127.0.0.1 serving `files` (path → body), for the tests; 404
    /// for the rest. Returns the base URL.
    pub fn serve(files: BTreeMap<String, Vec<u8>>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut r = std::io::BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    continue;
                }
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let mut s = s;
                match files.get(&path) {
                    Some(b) => {
                        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len());
                        let _ = s.write_all(b);
                    }
                    None => {
                        let _ = write!(s, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
            }
        });
        format!("http://{addr}")
    }

    /// A signed package `demo` 1.0.0 with one library: (package bytes, manifest bytes, key).
    pub fn package() -> (Vec<u8>, Vec<u8>, plugin_pkg::SigningKey) {
        let src = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("lib")).unwrap();
        fs::write(src.path().join("lib/tsmod_demo.so"), b"library bytes").unwrap();
        let key = plugin_pkg::SigningKey::from_bytes(&[9; 32]);
        let f = plugin_pkg::build::file_entry(src.path(), "lib/tsmod_demo.so", None).unwrap();
        let m = format!(
            "format = 1\nid = \"demo\"\nname = \"Demo\"\nversion = \"1.0.0\"\nclient_ext_api = 1\n\n[publisher]\nname = \"Pub\"\nkey = \"{}\"\n\n[platforms]\nlinux-x86_64 = \"lib/tsmod_demo.so\"\n\n[[files]]\npath = \"{}\"\nsize = {}\nsha256 = \"{}\"\n",
            PublicKey::of(&key),
            f.path,
            f.size,
            f.sha256
        );
        let sig = plugin_pkg::sign_manifest(&key, m.as_bytes());
        let mut out = std::io::Cursor::new(Vec::new());
        plugin_pkg::build::write_package(&mut out, m.as_bytes(), Some(&sig), None, src.path()).unwrap();
        (out.into_inner(), m.into_bytes(), key)
    }

    fn job(base: &str, dir: &Path, pkg: &[u8], manifest: &[u8], key: &plugin_pkg::SigningKey) -> InstallJob {
        InstallJob {
            url: format!("{base}/plugins/demo-1.0.0.tspkg"),
            plain_http: true,
            size: pkg.len() as u32,
            sha256: Sha256::of_bytes(pkg).0,
            manifest_sha256: Sha256::of_bytes(manifest),
            key: Some(PublicKey::of(key)),
            staging: dir.join("cl_plugins/staging/demo-1.0.0.tspkg.part"),
            dest: dir.join("cl_plugins/demo/1.0.0"),
            reuse: false,
            platform: Platform::LinuxX86_64,
            roots: Vec::new(),
            store: Store::new(),
        }
    }

    #[test]
    fn fetch_and_install() {
        let (pkg, manifest, key) = package();
        let sig = plugin_pkg::sign_manifest(&key, &manifest).to_vec();
        let mut files = BTreeMap::new();
        files.insert("/plugins/demo-1.0.0.tspkg".to_string(), pkg.clone());
        files.insert("/plugins/demo-1.0.0.manifest.toml".to_string(), manifest.clone());
        files.insert("/plugins/demo-1.0.0.manifest.sig".to_string(), sig.clone());
        files.insert("/plugins/bad-1.0.0.tspkg".to_string(), {
            let mut b = pkg.clone();
            let n = b.len() / 2;
            b[n] ^= 1;
            b
        });
        let base = serve(files);
        let t = Transport { base: format!("{base}/plugins/demo-1.0.0"), plain_http: true };
        let f = fetch_manifest(&t).unwrap();
        assert_eq!((f.manifest.as_slice(), f.signature.as_deref(), f.cert), (manifest.as_slice(), Some(sig.as_slice()), None));
        // HTTPS only refuses the plain URL.
        assert!(fetch_manifest(&Transport { plain_http: false, ..t.clone() }).is_err());
        assert!(fetch_manifest(&Transport { base: format!("{base}/missing"), plain_http: true }).is_err());

        let dir = tempfile::tempdir().unwrap();
        let j = job(&base, dir.path(), &pkg, &manifest, &key);
        let v = install(&j, &Progress::default()).unwrap();
        assert_eq!(v.id, "demo");
        assert!(v.files.contains_key("files/lib/tsmod_demo.so") && v.files.contains_key("manifest.sig"));
        assert!(!j.staging.exists(), "the staging file goes");
        // Installing again replaces the directory (the old one is set aside).
        install(&j, &Progress::default()).unwrap();
        assert!(dir.path().join("cl_plugins/demo/.1.0.0.replaced").is_dir());

        // Each refusal leaves nothing behind.
        let fresh = tempfile::tempdir().unwrap();
        let check = |j: InstallJob, why: &str| {
            let e = install(&j, &Progress::default()).unwrap_err();
            assert!(e.contains(why), "{e}");
            assert!(!j.dest.exists() && !j.staging.exists(), "{why}: nothing left");
        };
        check(InstallJob { url: format!("{base}/plugins/bad-1.0.0.tspkg"), ..job(&base, fresh.path(), &pkg, &manifest, &key) }, "SHA-256");
        check(InstallJob { size: pkg.len() as u32 - 1, ..job(&base, fresh.path(), &pkg, &manifest, &key) }, "offer says");
        check(InstallJob { manifest_sha256: Sha256::of_bytes(b"x"), ..job(&base, fresh.path(), &pkg, &manifest, &key) }, "manifest");
        check(InstallJob { key: None, ..job(&base, fresh.path(), &pkg, &manifest, &key) }, "signer");
        check(InstallJob { plain_http: false, ..job(&base, fresh.path(), &pkg, &manifest, &key) }, "download");
        let p = Progress::default();
        p.cancel.store(true, Ordering::Relaxed);
        let j = job(&base, fresh.path(), &pkg, &manifest, &key);
        assert_eq!(install(&j, &p).unwrap_err(), "cancelled");
        assert!(!j.staging.exists());
    }
}
