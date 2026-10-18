//! Which `tsmod_*` library files a game library may load: the naming and header rules the
//! server's plugin loader (`dlls/`, docs/implementation/plugin-api.md §1.2) and the client's
//! extension loader (`cl_dlls/`, docs/implementation/client-ext-api.md §2) share. Port addition.
//!
//! The naming rule: `tsmod_<name>` followed by the platform suffix (`.dll`, `_amd64.dll`,
//! `_i386.so`, `_amd64.so`; the server library's naming). `<name>` is letters, digits, `_` and
//! `-`. A `tsmod_` file with another platform's suffix, or whose header says it was built for
//! another platform or architecture, is skipped with a warning. Files load in name order. Opening
//! the library is the `*-dll` crates' business; everything decided before it is here, so it can
//! be tested without an engine.

use std::path::{Path, PathBuf};

/// The prefix of every plugin library.
pub const LIBRARY_PREFIX: &str = "tsmod_";

/// A platform and architecture the port builds its libraries for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub windows: bool,
    pub x86_64: bool,
}

impl Target {
    /// The platform this library was built for.
    pub const HOST: Target = Target { windows: cfg!(windows), x86_64: cfg!(target_arch = "x86_64") };
    pub const ALL: [Target; 4] = [
        Target { windows: true, x86_64: false },
        Target { windows: true, x86_64: true },
        Target { windows: false, x86_64: false },
        Target { windows: false, x86_64: true },
    ];

    /// The file name suffix (the same as `ts_plugin_api::library_suffix` and
    /// `ts_client_ext_api::library_suffix`).
    pub fn suffix(self) -> &'static str {
        match (self.windows, self.x86_64) {
            (true, false) => ".dll",
            (true, true) => "_amd64.dll",
            (false, false) => "_i386.so",
            (false, true) => "_amd64.so",
        }
    }

    /// For messages: "win32", "linux-amd64", ...
    pub fn describe(self) -> &'static str {
        match (self.windows, self.x86_64) {
            (true, false) => "win32",
            (true, true) => "win64",
            (false, false) => "linux-i386",
            (false, true) => "linux-amd64",
        }
    }
}

/// What a file is to the loader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Not a plugin file at all (another prefix, or no library suffix): ignored silently.
    NotPlugin,
    /// A plugin `name` for this platform: load it.
    Load(String),
    /// A plugin built for another platform or architecture (by its suffix).
    OtherTarget(Target),
    /// `tsmod_` with this platform's suffix, but no usable name between them.
    BadName,
}

/// [`Verdict`] of the file name `file` for a library built for `host`. The longest suffix that
/// matches decides, so `tsmod_x_amd64.dll` is a win64 plugin, not a win32 one named `x_amd64`.
/// Windows compares names case-insensitively, as its file system does.
pub fn classify(file: &str, host: Target) -> Verdict {
    let fold = |s: &str| if host.windows { s.to_ascii_lowercase() } else { s.to_owned() };
    let f = fold(file);
    if !f.starts_with(LIBRARY_PREFIX) {
        return Verdict::NotPlugin;
    }
    let Some(t) = Target::ALL.iter().filter(|t| f.ends_with(t.suffix())).max_by_key(|t| t.suffix().len()).copied() else {
        return Verdict::NotPlugin;
    };
    if t != host {
        return Verdict::OtherTarget(t);
    }
    let name = &file[LIBRARY_PREFIX.len()..file.len() - t.suffix().len()];
    if valid_name(name) { Verdict::Load(name.to_owned()) } else { Verdict::BadName }
}

/// A plugin name: 1 to 32 ASCII letters, digits, `_` or `-`.
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The platform and architecture an ELF or PE image was built for, from its first bytes (at
/// least the ELF header, or the DOS header and the PE header it points to): `None` if it is
/// neither, or for a machine the port doesn't build for.
pub fn image_target(bytes: &[u8]) -> Option<Target> {
    if bytes.starts_with(b"\x7fELF") {
        // e_ident[EI_DATA] must be little-endian; e_machine is the u16 at 0x12.
        if bytes.get(5) != Some(&1) {
            return None;
        }
        let machine = u16::from_le_bytes([*bytes.get(0x12)?, *bytes.get(0x13)?]);
        return match (bytes.get(4), machine) {
            (Some(1), 3) => Some(Target { windows: false, x86_64: false }), // ELFCLASS32, EM_386
            (Some(2), 62) => Some(Target { windows: false, x86_64: true }), // ELFCLASS64, EM_X86_64
            _ => None,
        };
    }
    if bytes.starts_with(b"MZ") {
        let at = u32::from_le_bytes(bytes.get(0x3c..0x40)?.try_into().ok()?) as usize;
        if bytes.get(at..at + 4)? != b"PE\0\0" {
            return None;
        }
        let machine = u16::from_le_bytes(bytes.get(at + 4..at + 6)?.try_into().ok()?);
        return match machine {
            0x14c => Some(Target { windows: true, x86_64: false }), // IMAGE_FILE_MACHINE_I386
            0x8664 => Some(Target { windows: true, x86_64: true }), // IMAGE_FILE_MACHINE_AMD64
            _ => None,
        };
    }
    None
}

/// How many bytes of a file [`image_target`] needs at most (the PE header may sit past the DOS
/// stub; MinGW and MSVC put it below 0x200).
pub const HEADER_BYTES: usize = 4096;

/// A file the loader looked at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub file: String,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Load it as plugin `name`.
    Load(String),
    /// Skip it, with this warning.
    Skip(String),
}

/// The plugin files in `dir` for `host`, in name order (files that aren't plugins left out). A
/// matching name is checked against the file's header too, so a library renamed for the wrong
/// platform is skipped before the system loader sees it. `side` names the loading library in the
/// warnings ("server", "client").
pub fn scan(dir: &Path, host: Target, side: &str) -> Vec<Found> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|(file, path)| decide(&file, &path, host, side, read_header).map(|outcome| Found { path, file, outcome }))
        .collect()
}

fn read_header(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::with_capacity(HEADER_BYTES);
    std::fs::File::open(path).ok()?.take(HEADER_BYTES as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// One file's [`Outcome`]; `None` for a file that isn't a plugin. `header` reads the file's first
/// bytes; `side` is as in [`scan`].
pub fn decide(file: &str, path: &Path, host: Target, side: &str, header: impl FnOnce(&Path) -> Option<Vec<u8>>) -> Option<Outcome> {
    match classify(file, host) {
        Verdict::NotPlugin => None,
        Verdict::BadName => {
            Some(Outcome::Skip(format!("{file}: not a plugin name (tsmod_<name>{}, <name> of letters, digits, _ and -)", host.suffix())))
        }
        Verdict::OtherTarget(t) => {
            Some(Outcome::Skip(format!("{file}: a {} library (this {side} is {}), skipped", t.describe(), host.describe())))
        }
        Verdict::Load(name) => Some(match header(path).as_deref().map(image_target) {
            None => Outcome::Skip(format!("{file}: can't be read, skipped")),
            Some(None) => Outcome::Skip(format!("{file}: not a library for this {side} ({}), skipped", host.describe())),
            Some(Some(t)) if t != host => {
                Outcome::Skip(format!("{file}: built for {} (this {side} is {}), skipped", t.describe(), host.describe()))
            }
            Some(Some(_)) => Outcome::Load(name),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_side_names_the_loader() {
        let host = Target { windows: false, x86_64: true };
        let p = Path::new("cl_dlls/tsmod_x.dll");
        let o = decide("tsmod_x.dll", p, host, "client", |_| None).unwrap();
        assert_eq!(o, Outcome::Skip("tsmod_x.dll: a win32 library (this client is linux-amd64), skipped".into()));
        let o = decide("tsmod_x_amd64.so", p, host, "client", |_| Some(b"junk".to_vec())).unwrap();
        assert!(matches!(&o, Outcome::Skip(w) if w.contains("not a library for this client")), "{o:?}");
    }

    #[test]
    fn suffixes_match_the_abi_crates() {
        for t in Target::ALL {
            assert!(t.suffix().ends_with(if t.windows { ".dll" } else { ".so" }));
        }
        assert_eq!(Target::ALL.iter().filter(|t| **t == Target::HOST).count(), 1);
    }
}
