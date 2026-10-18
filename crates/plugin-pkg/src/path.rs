//! Path rules for package files (client-plugins.md §3.2, §10).
//!
//! A path names a file inside a package, the same way on every platform. The rules are strict
//! so that a path can't escape the package directory and means the same file on Linux and on
//! Windows (whose file systems are case-insensitive and give some names special meaning):
//!
//! * relative, `/`-separated, at most [`MAX_PATH_LEN`] bytes;
//! * printable ASCII only, without `\ : * ? " < > |` (so no drive letters or NTFS streams);
//! * no empty, `.` or `..` segments, and no segment ending in `.` or a space;
//! * no Windows device names (`CON`, `NUL`, `COM1`, ..., with or without an extension);
//! * within one package, no two paths equal ignoring ASCII case, and no path that is also
//!   another path's directory.

use std::collections::HashMap;
use std::fmt;

/// The longest file path a package may use, in bytes. Windows' 260-character `MAX_PATH` also
/// has to fit the game directory and `cl_plugins/<id>/<version>/files/`.
pub const MAX_PATH_LEN: usize = 160;

/// Why a path is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathError {
    /// The empty path.
    Empty,
    /// Longer than [`MAX_PATH_LEN`].
    TooLong(String),
    /// Starts with `/`.
    Absolute(String),
    /// Contains `\`; paths use `/`.
    Backslash(String),
    /// Contains `:`: a drive letter (`C:`), or an NTFS stream.
    Colon(String),
    /// A character outside printable ASCII, or one Windows forbids in names.
    BadChar(String, char),
    /// An empty segment (`a//b`, a trailing `/`), `.` or `..`.
    BadSegment(String),
    /// A segment ending in `.` or a space, which Windows silently strips.
    TrailingDotOrSpace(String),
    /// A Windows device name such as `CON` or `com1.txt`.
    Reserved(String),
    /// The same path twice.
    Duplicate(String),
    /// Two paths that differ only in case.
    CaseCollision(String, String),
    /// A path that is also used as a directory of another path.
    FileIsDirectory(String),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::Empty => write!(f, "empty path"),
            PathError::TooLong(p) => write!(f, "path longer than {MAX_PATH_LEN} bytes: {p}"),
            PathError::Absolute(p) => write!(f, "absolute path: {p}"),
            PathError::Backslash(p) => write!(f, "path uses '\\': {p}"),
            PathError::Colon(p) => write!(f, "path has a drive letter or ':': {p}"),
            PathError::BadChar(p, c) => write!(f, "path has the character {c:?}: {p}"),
            PathError::BadSegment(p) => write!(f, "path has an empty, '.' or '..' segment: {p}"),
            PathError::TrailingDotOrSpace(p) => write!(f, "path segment ends in '.' or a space: {p}"),
            PathError::Reserved(p) => write!(f, "path uses a Windows device name: {p}"),
            PathError::Duplicate(p) => write!(f, "path listed twice: {p}"),
            PathError::CaseCollision(a, b) => write!(f, "paths differ only in case: {a}, {b}"),
            PathError::FileIsDirectory(p) => write!(f, "path is both a file and a directory: {p}"),
        }
    }
}

impl std::error::Error for PathError {}

const RESERVED: &[&str] = &["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

fn is_reserved(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or(segment).trim_end_matches(' ').to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return true;
    }
    let b = stem.as_bytes();
    b.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && b[3].is_ascii_digit()
}

/// Checks one path against the rules in the module docs.
pub fn check_path(path: &str) -> Result<(), PathError> {
    let own = || path.to_string();
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    if path.len() > MAX_PATH_LEN {
        return Err(PathError::TooLong(own()));
    }
    if path.starts_with('/') {
        return Err(PathError::Absolute(own()));
    }
    for c in path.chars() {
        match c {
            '\\' => return Err(PathError::Backslash(own())),
            ':' => return Err(PathError::Colon(own())),
            '*' | '?' | '"' | '<' | '>' | '|' => return Err(PathError::BadChar(own(), c)),
            c if !c.is_ascii() || c.is_ascii_control() => return Err(PathError::BadChar(own(), c)),
            _ => {}
        }
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(PathError::BadSegment(own()));
        }
        if segment.ends_with('.') || segment.ends_with(' ') {
            return Err(PathError::TrailingDotOrSpace(own()));
        }
        if is_reserved(segment) {
            return Err(PathError::Reserved(own()));
        }
    }
    Ok(())
}

/// Checks a set of paths together: each with [`check_path`], then no duplicates, no two that
/// differ only in case, and no path that is another's directory.
pub fn check_path_set<'a>(paths: impl IntoIterator<Item = &'a str>) -> Result<(), PathError> {
    let mut files: HashMap<String, &'a str> = HashMap::new();
    for p in paths {
        check_path(p)?;
        if let Some(prev) = files.insert(p.to_ascii_lowercase(), p) {
            return Err(if prev == p { PathError::Duplicate(p.into()) } else { PathError::CaseCollision(prev.into(), p.into()) });
        }
    }
    for p in files.values() {
        let lower = p.to_ascii_lowercase();
        for (i, _) in lower.match_indices('/') {
            if let Some(file) = files.get(&lower[..i]) {
                return Err(PathError::FileIsDirectory((*file).into()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_paths() {
        for p in ["a", "lib/linux-x86_64/tsmod_webview.so", "bin/tsmod-webview-host", "locales/en-US.pak", ".hidden", "a b/c"] {
            assert_eq!(check_path(p), Ok(()), "{p}");
        }
    }

    #[test]
    fn refuses_escapes_and_platform_traps() {
        type Case = (&'static str, fn(&PathError) -> bool);
        let cases: &[Case] = &[
            ("", |e| matches!(e, PathError::Empty)),
            ("/etc/passwd", |e| matches!(e, PathError::Absolute(_))),
            ("../x", |e| matches!(e, PathError::BadSegment(_))),
            ("a/../../x", |e| matches!(e, PathError::BadSegment(_))),
            ("a/./b", |e| matches!(e, PathError::BadSegment(_))),
            ("a//b", |e| matches!(e, PathError::BadSegment(_))),
            ("a/", |e| matches!(e, PathError::BadSegment(_))),
            ("C:/Windows/x.dll", |e| matches!(e, PathError::Colon(_))),
            ("c:x", |e| matches!(e, PathError::Colon(_))),
            ("a.dll:stream", |e| matches!(e, PathError::Colon(_))),
            ("a\\..\\b", |e| matches!(e, PathError::Backslash(_))),
            ("\\\\server\\share", |e| matches!(e, PathError::Backslash(_))),
            ("a\0b", |e| matches!(e, PathError::BadChar(..))),
            ("a\nb", |e| matches!(e, PathError::BadChar(..))),
            ("caf\u{e9}", |e| matches!(e, PathError::BadChar(..))),
            ("a*b", |e| matches!(e, PathError::BadChar(..))),
            ("dir./x", |e| matches!(e, PathError::TrailingDotOrSpace(_))),
            ("x ", |e| matches!(e, PathError::TrailingDotOrSpace(_))),
            ("CON", |e| matches!(e, PathError::Reserved(_))),
            ("lib/nul.txt", |e| matches!(e, PathError::Reserved(_))),
            ("com1", |e| matches!(e, PathError::Reserved(_))),
            ("LPT9.dll", |e| matches!(e, PathError::Reserved(_))),
        ];
        for (p, ok) in cases {
            let e = check_path(p).expect_err(p);
            assert!(ok(&e), "{p:?}: {e:?}");
        }
        assert!(check_path(&"a".repeat(MAX_PATH_LEN + 1)).is_err());
        assert!(check_path(&"a".repeat(MAX_PATH_LEN)).is_ok());
        assert!(check_path("com10").is_ok());
        assert!(check_path("console").is_ok());
    }

    #[test]
    fn set_rules() {
        assert_eq!(check_path_set(["a", "b/c", "b/d"]), Ok(()));
        assert_eq!(check_path_set(["a", "a"]), Err(PathError::Duplicate("a".into())));
        assert!(matches!(check_path_set(["lib/A.so", "lib/a.so"]), Err(PathError::CaseCollision(..))));
        assert_eq!(check_path_set(["b", "b/c"]), Err(PathError::FileIsDirectory("b".into())));
        assert_eq!(check_path_set(["B", "b/c"]), Err(PathError::FileIsDirectory("B".into())));
        assert!(matches!(check_path_set(["ok", "../bad"]), Err(PathError::BadSegment(_))));
    }
}
