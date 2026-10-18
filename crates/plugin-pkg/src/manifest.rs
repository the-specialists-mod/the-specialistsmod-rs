//! `manifest.toml` (client-plugins.md §3.2, §13.2).

use std::collections::BTreeMap;
use std::fmt;

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::hash::Sha256;
use crate::path::{PathError, check_path, check_path_set};
use crate::platform::Platform;
use crate::sign::PublicKey;

/// The manifest format this build reads and writes.
pub const FORMAT: u32 = 1;
/// The largest manifest accepted, in bytes (§6.3: the manifest is fetched before the package).
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// The longest plugin id (the `TSPkg` message's limit, §6.2).
pub const MAX_ID_LEN: usize = 32;
/// The longest version string (the `TSPkg` message's limit, §6.2).
pub const MAX_VERSION_LEN: usize = 16;
/// The most a package may unpack to on one platform.
pub const MAX_INSTALLED_BYTES: u64 = 4 << 30;

/// A package's manifest. [`Manifest::parse`] is the only way in from bytes, and it validates;
/// a value built by hand should go through [`Manifest::validate`] before it's written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Always [`FORMAT`].
    pub format: u32,
    /// `[a-z0-9_-]{1,32}`. The library is `tsmod_<id><suffix>`.
    pub id: String,
    /// The display name.
    pub name: String,
    /// Semver, at most [`MAX_VERSION_LEN`] characters.
    pub version: Version,
    /// What the plugin says it does, shown on the cards.
    #[serde(default)]
    pub description: String,
    /// An `https://` (or `http://`) page, shown on the cards and in kick reasons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// SPDX license expression, informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// The client extension API version the plugin was built for; must match exactly (§9).
    pub client_ext_api: u32,
    /// Who publishes it. Absent or without a key for unsigned packages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<Publisher>,
    /// What the plugin needs from the port (§13.5). Enforced by the loader, not here.
    #[serde(default, skip_serializing_if = "Requires::is_empty")]
    pub requires: Requires,
    /// The library to load per platform, as a path in `files`.
    pub platforms: BTreeMap<Platform, String>,
    /// Informational: shown on the cards, not enforced (§1).
    #[serde(default)]
    pub declares: Declares,
    /// Every file in the package, no exceptions.
    #[serde(default)]
    pub files: Vec<FileEntry>,
}

/// `[publisher]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    /// The publisher's name. Shown as "Verified publisher: `name`" only when `publisher.cert`
    /// countersigns this exact name.
    pub name: String,
    /// The key `manifest.sig` verifies with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<PublicKey>,
}

/// `[requires]`: what the plugin needs. Port addition to §3.2's example (§13.5 refers to it).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    /// A semver requirement on the port client's version (`_tsrs`), such as `">=3.0.8, <3.1"`.
    /// A plugin whose requirement the running port doesn't meet is skipped at start, with the
    /// reason, and isn't offered for approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<VersionReq>,
}

impl Requires {
    /// No requirements.
    pub fn is_empty(&self) -> bool {
        self.port.is_none()
    }
}

/// `[declares]`: informational only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declares {
    /// Executables the plugin starts (paths in `files`), through `verified_spawn`.
    #[serde(default)]
    pub processes: Vec<String>,
    /// Uses the network.
    #[serde(default)]
    pub network: bool,
    /// Writes to its data directory.
    #[serde(default)]
    pub data: bool,
}

/// One `[[files]]` entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// The path under `files/` in the package (§3.2 path rules).
    pub path: String,
    /// Bytes.
    pub size: u64,
    /// SHA-256 of the contents.
    pub sha256: Sha256,
    /// Only installed on this platform; absent: on every platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
}

impl FileEntry {
    /// Whether the file is installed on `platform`.
    pub fn is_for(&self, platform: Platform) -> bool {
        self.platform.is_none_or(|p| p == platform)
    }
}

/// Why a manifest is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// Larger than [`MAX_MANIFEST_BYTES`].
    TooLarge(u64),
    /// Not UTF-8.
    NotUtf8,
    /// Not valid TOML, or not the manifest's shape (unknown keys are refused too).
    Toml(String),
    /// A `format` this build doesn't read.
    Format(u32),
    /// A field breaks a rule.
    Field(&'static str, String),
    /// A file path breaks the path rules.
    Path(PathError),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::TooLarge(n) => write!(f, "manifest is {n} bytes, more than {MAX_MANIFEST_BYTES}"),
            ManifestError::NotUtf8 => write!(f, "manifest isn't UTF-8"),
            ManifestError::Toml(e) => write!(f, "manifest doesn't parse: {e}"),
            ManifestError::Format(n) => write!(f, "manifest format {n} (this build reads {FORMAT})"),
            ManifestError::Field(field, problem) => write!(f, "manifest {field}: {problem}"),
            ManifestError::Path(e) => write!(f, "manifest files: {e}"),
        }
    }
}

impl std::error::Error for ManifestError {}

impl From<PathError> for ManifestError {
    fn from(e: PathError) -> Self {
        ManifestError::Path(e)
    }
}

/// `[a-z0-9_-]{1,32}`: a plugin id, and a hand-installed library's `<name>`.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn check_text(field: &'static str, s: &str, max: usize, multiline: bool) -> Result<(), ManifestError> {
    if s.len() > max {
        return Err(ManifestError::Field(field, format!("longer than {max} bytes")));
    }
    // Control characters could reorder or hide text on the cards (bidi overrides included).
    let bad = |c: char| (c.is_control() && !(multiline && c == '\n')) || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
    if s.chars().any(bad) {
        return Err(ManifestError::Field(field, "has control characters".into()));
    }
    Ok(())
}

impl Manifest {
    /// Parses and validates `manifest.toml`'s bytes.
    pub fn parse(bytes: &[u8]) -> Result<Manifest, ManifestError> {
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge(bytes.len() as u64));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| ManifestError::NotUtf8)?;
        let manifest: Manifest = toml::from_str(text).map_err(|e| {
            // `format` first, so a newer format reports that rather than its unknown keys.
            match toml::from_str::<toml::Table>(text).ok().and_then(|t| t.get("format").and_then(|v| v.as_integer())) {
                Some(n) if n != i64::from(FORMAT) => ManifestError::Format(n.clamp(0, i64::from(u32::MAX)) as u32),
                _ => ManifestError::Toml(e.message().to_string()),
            }
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Checks every rule a parsed manifest must meet.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.format != FORMAT {
            return Err(ManifestError::Format(self.format));
        }
        if !is_valid_id(&self.id) {
            return Err(ManifestError::Field("id", format!("{:?} isn't [a-z0-9_-]{{1,{MAX_ID_LEN}}}", self.id)));
        }
        if self.version.to_string().len() > MAX_VERSION_LEN {
            return Err(ManifestError::Field("version", format!("longer than {MAX_VERSION_LEN} characters")));
        }
        if self.name.trim().is_empty() {
            return Err(ManifestError::Field("name", "empty".into()));
        }
        check_text("name", &self.name, 64, false)?;
        check_text("description", &self.description, 1024, true)?;
        if let Some(license) = &self.license {
            check_text("license", license, 64, false)?;
        }
        if let Some(home) = &self.homepage {
            check_text("homepage", home, 256, false)?;
            if !(home.starts_with("https://") || home.starts_with("http://")) || home.contains(char::is_whitespace) {
                return Err(ManifestError::Field("homepage", "must be an http(s):// URL".into()));
            }
        }
        if let Some(p) = &self.publisher {
            if p.name.trim().is_empty() {
                return Err(ManifestError::Field("publisher.name", "empty".into()));
            }
            check_text("publisher.name", &p.name, 64, false)?;
        }
        check_path_set(self.files.iter().map(|f| f.path.as_str()))?;
        if self.platforms.is_empty() {
            return Err(ManifestError::Field("platforms", "no library for any platform".into()));
        }
        for (&platform, lib) in &self.platforms {
            check_path(lib)?;
            match self.file(lib) {
                Some(f) if f.is_for(platform) => {}
                Some(_) => return Err(ManifestError::Field("platforms", format!("{lib} isn't installed on {platform}"))),
                None => return Err(ManifestError::Field("platforms", format!("{lib} isn't in files"))),
            }
            let total = self.files_for(platform).try_fold(0u64, |sum, f| sum.checked_add(f.size));
            if total.is_none_or(|t| t > MAX_INSTALLED_BYTES) {
                return Err(ManifestError::Field("files", format!("more than {MAX_INSTALLED_BYTES} bytes on {platform}")));
            }
        }
        for p in &self.declares.processes {
            if self.file(p).is_none() {
                return Err(ManifestError::Field("declares.processes", format!("{p} isn't in files")));
            }
        }
        Ok(())
    }

    /// The manifest as TOML. Sign the bytes that are written, not a re-serialization.
    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// The `[[files]]` entry for `path`.
    pub fn file(&self, path: &str) -> Option<&FileEntry> {
        self.files.iter().find(|f| f.path == path)
    }

    /// The files installed on `platform`.
    pub fn files_for(&self, platform: Platform) -> impl Iterator<Item = &FileEntry> {
        self.files.iter().filter(move |f| f.is_for(platform))
    }

    /// The library to load on `platform`, as a path under `files/`.
    pub fn library(&self, platform: Platform) -> Option<&str> {
        self.platforms.get(&platform).map(String::as_str)
    }

    /// Whether the package has a library for `platform`; if not it's `unsupported` (§6.3).
    pub fn supports(&self, platform: Platform) -> bool {
        self.platforms.contains_key(&platform)
    }

    /// The bytes installed on `platform` (the "installed size" on the cards).
    pub fn installed_size(&self, platform: Platform) -> u64 {
        self.files_for(platform).map(|f| f.size).sum()
    }

    /// Whether the running port's version meets `[requires] port`. True when there's none.
    pub fn port_requirement_met(&self, port: &Version) -> bool {
        self.requires.port.as_ref().is_none_or(|req| req.matches(port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{file_toml, key, manifest_text, publisher_toml};

    fn parse(text: &str) -> Result<Manifest, ManifestError> {
        Manifest::parse(text.as_bytes())
    }

    #[test]
    fn parses_the_spec_example() {
        let k = PublicKey::of(&key(1));
        let text = format!(
            r#"format = 1
id = "webview"
name = "Web overlay"
version = "0.1.0"
description = "A Chromium overlay that servers and the player can open."
homepage = "https://github.com/the-specialists-mod/tsmod-plugin-webview-rs"
license = "MIT"
client_ext_api = 1

[publisher]
name = "The Specialists Mod"
key = "{k}"

[requires]
port = ">=3.0.8, <3.1"

[platforms]
linux-x86_64 = "lib/linux-x86_64/tsmod_webview.so"
windows-i686 = "lib/windows-i686/tsmod_webview.dll"

[declares]
processes = ["bin/tsmod-webview-host"]
network = true
data = true

[[files]]
path = "lib/linux-x86_64/tsmod_webview.so"
size = 2412544
sha256 = "{h}"
platform = "linux-x86_64"

[[files]]
path = "lib/windows-i686/tsmod_webview.dll"
size = 10
sha256 = "{h}"
platform = "windows-i686"

[[files]]
path = "bin/tsmod-webview-host"
size = 5
sha256 = "{h}"
"#,
            h = Sha256::of_bytes(b"x")
        );
        let m = parse(&text).unwrap();
        assert_eq!(m.id, "webview");
        assert_eq!(m.publisher.as_ref().unwrap().key, Some(k));
        assert_eq!(m.library(Platform::LinuxX86_64), Some("lib/linux-x86_64/tsmod_webview.so"));
        assert!(!m.supports(Platform::LinuxI686));
        assert_eq!(m.files_for(Platform::LinuxX86_64).count(), 2);
        assert_eq!(m.installed_size(Platform::WindowsI686), 15);
        assert!(m.port_requirement_met(&Version::new(3, 0, 8)));
        assert!(!m.port_requirement_met(&Version::new(3, 0, 7)));
        assert!(!m.port_requirement_met(&Version::new(3, 1, 0)));
        // to_toml round-trips.
        assert_eq!(parse(&m.to_toml()).unwrap(), m);
    }

    #[test]
    fn refuses_bad_fields() {
        let ok = manifest_text("demo", "1.0.0", &publisher_toml("P", None), &[]);
        assert!(parse(&ok).is_ok());
        let bad = |from: &str, to: &str| parse(&ok.replacen(from, to, 1)).unwrap_err();
        assert!(matches!(bad("id = \"demo\"", "id = \"Demo\""), ManifestError::Field("id", _)));
        assert!(matches!(bad("id = \"demo\"", "id = \"\""), ManifestError::Field("id", _)));
        assert!(matches!(bad("id = \"demo\"", &format!("id = \"{}\"", "a".repeat(33))), ManifestError::Field("id", _)));
        assert!(matches!(bad("id = \"demo\"", "id = \"manual:x\""), ManifestError::Field("id", _)));
        assert!(matches!(bad("version = \"1.0.0\"", "version = \"v1\""), ManifestError::Toml(_)));
        assert!(matches!(bad("version = \"1.0.0\"", "version = \"1.0.0-alpha.123456\""), ManifestError::Field("version", _)));
        assert!(matches!(bad("format = 1", "format = 2"), ManifestError::Format(2)));
        assert!(matches!(bad("format = 1", "format = 2\nnew_thing = 1"), ManifestError::Format(2)));
        assert!(matches!(bad("client_ext_api = 1", "client_ext_api = 1\nsurprise = 1"), ManifestError::Toml(_)));
        assert!(matches!(bad("name = \"Demo\"", "name = \"a\\u202eb\""), ManifestError::Field("name", _)));
        assert!(matches!(bad("name = \"Demo\"", "name = \" \""), ManifestError::Field("name", _)));
        assert!(matches!(
            bad("client_ext_api = 1", "client_ext_api = 1\nhomepage = \"javascript:x\""),
            ManifestError::Field("homepage", _)
        ));
        assert!(matches!(bad("client_ext_api = 1", "client_ext_api = 1\n[requires]\nport = \"nope\""), ManifestError::Toml(_)));
        assert!(matches!(Manifest::parse(&[0xff, 0xfe]), Err(ManifestError::NotUtf8)));
        let huge = format!("{ok}\n#{}", "x".repeat(MAX_MANIFEST_BYTES as usize));
        assert!(matches!(parse(&huge), Err(ManifestError::TooLarge(_))));
    }

    #[test]
    fn path_rules_apply_to_files_and_platforms() {
        let h = Sha256::of_bytes(b"");
        let with = |extra: &[(&str, Option<&str>)]| {
            let mut files = vec![file_toml("lib/tsmod_demo.so", 0, &h, None)];
            files.extend(extra.iter().map(|(p, plat)| file_toml(p, 0, &h, *plat)));
            parse(&manifest_text("demo", "1.0.0", "", &files))
        };
        assert!(with(&[("data/a.txt", None)]).is_ok());
        for bad in ["../evil", "/etc/passwd", "C:/x", "a\\b", "x/./y", "con.txt"] {
            assert!(matches!(with(&[(bad, None)]), Err(ManifestError::Path(_))), "{bad}");
        }
        assert!(matches!(with(&[("lib/tsmod_demo.so", None)]), Err(ManifestError::Path(PathError::Duplicate(_)))));
        assert!(matches!(with(&[("LIB/tsmod_demo.so", None)]), Err(ManifestError::Path(PathError::CaseCollision(..)))));
        assert!(matches!(with(&[("lib/tsmod_demo.so/x", None)]), Err(ManifestError::Path(PathError::FileIsDirectory(_)))));
        // The library must be listed, and installed on its platform.
        let missing_lib = manifest_text("demo", "1.0.0", "", &[file_toml("lib/other.so", 0, &h, None)]);
        assert!(matches!(parse(&missing_lib), Err(ManifestError::Field("platforms", _))));
        let wrong_platform = manifest_text("demo", "1.0.0", "", &[file_toml("lib/tsmod_demo.so", 0, &h, Some("windows-i686"))]);
        assert!(matches!(parse(&wrong_platform), Err(ManifestError::Field("platforms", _))));
        // So must every declared process.
        let undeclared = manifest_text("demo", "1.0.0", "", &[]).replace("network = false", "network = false\nprocesses = [\"bin/host\"]");
        assert!(matches!(parse(&undeclared), Err(ManifestError::Field("declares.processes", _))));
    }

    #[test]
    fn size_limits() {
        let h = Sha256::of_bytes(b"");
        let lib = file_toml("lib/tsmod_demo.so", 1, &h, None);
        let fits = manifest_text("demo", "1.0.0", "", &[lib.clone(), file_toml("big", MAX_INSTALLED_BYTES - 1, &h, None)]);
        assert!(parse(&fits).is_ok());
        let big = manifest_text("demo", "1.0.0", "", &[lib, file_toml("big", MAX_INSTALLED_BYTES, &h, None)]);
        assert!(matches!(parse(&big), Err(ManifestError::Field("files", _))));
        let overflow =
            manifest_text("demo", "1.0.0", "", &[file_toml("a", u64::MAX / 2 + 1, &h, None), file_toml("b", u64::MAX / 2 + 1, &h, None)]);
        assert!(parse(&overflow).is_err());
    }
}
