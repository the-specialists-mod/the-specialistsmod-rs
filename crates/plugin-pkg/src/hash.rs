//! SHA-256 over bytes and files (client-plugins.md §5.2, §10).
//!
//! Every verification hashes every file in full. There is deliberately no cache keyed on size or
//! mtime: a change that preserves them would slip past it.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::Digest;

/// A SHA-256 digest. Written as 64 lowercase hex digits in manifests and the store.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256(pub [u8; 32]);

impl Sha256 {
    /// The digest of `bytes`.
    pub fn of_bytes(bytes: &[u8]) -> Sha256 {
        Sha256(sha2::Sha256::digest(bytes).into())
    }

    /// Hashes everything `reader` yields. Returns the digest and the number of bytes read.
    pub fn of_reader(mut reader: impl Read) -> io::Result<(Sha256, u64)> {
        let mut hasher = sha2::Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        Ok((Sha256(hasher.finalize().into()), total))
    }

    /// Hashes a regular file. Symlinks and other special files are refused, so a package
    /// directory can't point a hash at something outside it.
    pub fn of_file(path: &Path) -> io::Result<(Sha256, u64)> {
        if !std::fs::symlink_metadata(path)?.file_type().is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
        }
        let file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
        }
        Sha256::of_reader(file)
    }

    /// 64 lowercase hex digits.
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
            s.push(char::from_digit(u32::from(b & 15), 16).unwrap_or('0'));
        }
        s
    }
}

impl FromStr for Sha256 {
    type Err = String;

    /// Parses 64 hex digits (either case).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let b = s.as_bytes();
        if b.len() != 64 {
            return Err(format!("a SHA-256 is 64 hex digits, not {:?}", s));
        }
        let mut out = [0u8; 32];
        for (i, pair) in b.chunks(2).enumerate() {
            let hi = (pair[0] as char).to_digit(16);
            let lo = (pair[1] as char).to_digit(16);
            match (hi, lo) {
                (Some(hi), Some(lo)) => out[i] = (hi * 16 + lo) as u8,
                _ => return Err(format!("{:?} isn't hex", s)),
            }
        }
        Ok(Sha256(out))
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256({})", self.to_hex())
    }
}

impl Serialize for Sha256 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Sha256 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// The result of hashing one file: its digest and size, or why it couldn't be read.
pub type FileHash = io::Result<(Sha256, u64)>;

/// Hashes `root/<rel>` for every `rel` (paths use `/`), on up to `threads` worker threads
/// (0: one per available core). The results are in the order of `rels`.
pub fn hash_files(root: &Path, rels: &[String], threads: usize) -> Vec<FileHash> {
    let threads = if threads == 0 { std::thread::available_parallelism().map_or(1, |n| n.get()) } else { threads };
    let threads = threads.clamp(1, rels.len().max(1));
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<FileHash>>> = Mutex::new((0..rels.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(rel) = rels.get(i) else { break };
                    let result = Sha256::of_file(&root.join(rel));
                    if let Ok(mut r) = results.lock() {
                        r[i] = Some(result);
                    }
                }
            });
        }
    });
    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    results.into_iter().map(|r| r.unwrap_or_else(|| Err(io::Error::other("hash worker failed")))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digest_and_hex_round_trip() {
        let h = Sha256::of_bytes(b"abc");
        assert_eq!(h.to_hex(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(h.to_hex().parse::<Sha256>().unwrap(), h);
        assert_eq!(h.to_hex().to_uppercase().parse::<Sha256>().unwrap(), h);
        assert!("abc".parse::<Sha256>().is_err());
        assert!("zz".repeat(32).parse::<Sha256>().is_err());
    }

    #[test]
    fn parallel_hashing_matches_serial_and_keeps_order() {
        let dir = tempfile::tempdir().unwrap();
        let rels: Vec<String> = (0..37).map(|i| format!("f{i}")).collect();
        for (i, r) in rels.iter().enumerate() {
            std::fs::write(dir.path().join(r), vec![i as u8; i * 1000]).unwrap();
        }
        let mut with_missing = rels.clone();
        with_missing.push("absent".into());
        let out = hash_files(dir.path(), &with_missing, 4);
        for (i, r) in out.iter().take(rels.len()).enumerate() {
            let (h, size) = r.as_ref().unwrap();
            assert_eq!(*h, Sha256::of_bytes(&vec![i as u8; i * 1000]));
            assert_eq!(*size, (i * 1000) as u64);
        }
        assert!(out.last().unwrap().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_hashed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        assert!(Sha256::of_file(&dir.path().join("link")).is_err());
        assert!(Sha256::of_file(&dir.path().join("real")).is_ok());
    }
}
