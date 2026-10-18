//! Client platforms: which library a package loads (client-plugins.md §3.2).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A client platform, named as in the manifest's `[platforms]` table and the release assets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Platform {
    /// `linux-i686`
    LinuxI686,
    /// `linux-x86_64`
    LinuxX86_64,
    /// `windows-i686`
    WindowsI686,
    /// `windows-x86_64`
    WindowsX86_64,
}

impl Platform {
    /// Every platform, in manifest order.
    pub const ALL: [Platform; 4] = [Platform::LinuxI686, Platform::LinuxX86_64, Platform::WindowsI686, Platform::WindowsX86_64];

    /// The name used in manifests and the store.
    pub fn name(self) -> &'static str {
        match self {
            Platform::LinuxI686 => "linux-i686",
            Platform::LinuxX86_64 => "linux-x86_64",
            Platform::WindowsI686 => "windows-i686",
            Platform::WindowsX86_64 => "windows-x86_64",
        }
    }

    /// The platform this code was built for, if it is one the port supports. The client
    /// passes its own; the launcher passes the installation's, which can differ from its own.
    pub fn current() -> Option<Platform> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86") => Some(Platform::LinuxI686),
            ("linux", "x86_64") => Some(Platform::LinuxX86_64),
            ("windows", "x86") => Some(Platform::WindowsI686),
            ("windows", "x86_64") => Some(Platform::WindowsX86_64),
            _ => None,
        }
    }

    /// The suffix of a hand-installed `tsmod_<name><suffix>` library (§5.3): the same as the
    /// server's `tsmod_*` plugins (`ts_plugin_api::library_suffix`), so one naming rule covers
    /// both sides.
    pub fn library_suffix(self) -> &'static str {
        match self {
            Platform::LinuxI686 => "_i386.so",
            Platform::LinuxX86_64 => "_amd64.so",
            Platform::WindowsI686 => ".dll",
            Platform::WindowsX86_64 => "_amd64.dll",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Platform {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Platform::ALL.into_iter().find(|p| p.name() == s).ok_or_else(|| format!("unknown platform {s:?}"))
    }
}

impl Serialize for Platform {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Platform {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for p in Platform::ALL {
            assert_eq!(p.name().parse::<Platform>(), Ok(p));
        }
        assert!("macos-arm64".parse::<Platform>().is_err());
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        assert_eq!(Platform::current(), Some(Platform::LinuxX86_64));
    }
}
