//! Which `tsmod_*` files to load (docs/implementation/plugin-api.md §1.2). The FFI (opening the
//! library, calling its `TSRS_PluginLoad`) is `ts-server-dll`'s; everything decided before and
//! after it is here, so it can be tested without an engine.
//!
//! The naming and header rules are `shared::plugin_lib`'s, which the client's extension loader
//! shares: in the server's `dlls/` directory, `tsmod_<name>` followed by the server library's own
//! platform suffix (`ts_plugin_api::library_suffix`): `tsmod_<name>.dll` next to `mp.dll`,
//! `tsmod_<name>_amd64.dll` next to `mp_amd64.dll`, `tsmod_<name>_i386.so` next to `ts_i386.so`
//! and `tsmod_<name>_amd64.so` next to `ts_amd64.so`. `<name>` is letters, digits, `_` and `-`. A
//! `tsmod_` file with another platform's suffix, or whose header says it was built for another
//! platform or architecture, is skipped with a warning. Files load in name order.

pub use shared::plugin_lib::{Found, HEADER_BYTES, Outcome, Target, Verdict, classify, image_target, valid_name};
use std::ffi::c_int;
use std::path::Path;
use ts_plugin_api::{OK, Plugin, VERSION};

/// The plugin files in `dir` for `host`, in name order ([`shared::plugin_lib::scan`]).
pub fn scan(dir: &Path, host: Target) -> Vec<Found> {
    shared::plugin_lib::scan(dir, host, "server")
}

/// One file's [`Outcome`] ([`shared::plugin_lib::decide`]).
pub fn decide(file: &str, path: &Path, host: Target, header: impl FnOnce(&Path) -> Option<Vec<u8>>) -> Option<Outcome> {
    shared::plugin_lib::decide(file, path, host, "server", header)
}

/// The checks after the plugin's `TSRS_PluginLoad` returned `ret` and filled `desc`: it must have
/// returned `OK`, kept the descriptor's size and built for this API version.
pub fn check_loaded(ret: c_int, desc: &Plugin) -> Result<(), String> {
    if ret != OK {
        return Err(format!("its TSRS_PluginLoad refused to load ({ret})"));
    }
    if desc.size as usize != size_of::<Plugin>() {
        return Err(format!("its plugin descriptor is {} bytes, this server's is {}", desc.size, size_of::<Plugin>()));
    }
    if desc.api_version != VERSION {
        return Err(format!("built for plugin API version {}, this server has version {VERSION}", desc.api_version));
    }
    Ok(())
}

/// The descriptor the server hands a plugin to fill: its size set, nothing else (a plugin that
/// doesn't say which API version it was built for isn't loaded).
pub fn blank_descriptor() -> Plugin {
    Plugin { api_version: 0, ..Plugin::EMPTY }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W32: Target = Target { windows: true, x86_64: false };
    const W64: Target = Target { windows: true, x86_64: true };
    const L32: Target = Target { windows: false, x86_64: false };
    const L64: Target = Target { windows: false, x86_64: true };

    #[test]
    fn discovery_follows_the_server_library_naming() {
        assert_eq!(classify("tsmod_rcbot.dll", W32), Verdict::Load("rcbot".into()));
        assert_eq!(classify("tsmod_rcbot_amd64.dll", W64), Verdict::Load("rcbot".into()));
        assert_eq!(classify("tsmod_rcbot_i386.so", L32), Verdict::Load("rcbot".into()));
        assert_eq!(classify("tsmod_rcbot_amd64.so", L64), Verdict::Load("rcbot".into()));
        assert_eq!(classify("tsmod_test_badver_i386.so", L32), Verdict::Load("test_badver".into()));
        // Windows file names are case-insensitive; the name keeps its case.
        assert_eq!(classify("TSMOD_RcBot.DLL", W32), Verdict::Load("RcBot".into()));
        assert_eq!(classify("TSMOD_x_i386.so", L32), Verdict::NotPlugin, "Linux is case-sensitive");
    }

    #[test]
    fn other_platforms_are_recognised_and_skipped() {
        // The longest suffix wins: a win64 plugin isn't a win32 one named `x_amd64`.
        assert_eq!(classify("tsmod_x_amd64.dll", W32), Verdict::OtherTarget(W64));
        assert_eq!(classify("tsmod_x.dll", W64), Verdict::OtherTarget(W32));
        assert_eq!(classify("tsmod_x_i386.so", W32), Verdict::OtherTarget(L32));
        assert_eq!(classify("tsmod_x_amd64.so", L32), Verdict::OtherTarget(L64));
        assert_eq!(classify("tsmod_x_i386.so", L64), Verdict::OtherTarget(L32));
        assert_eq!(classify("tsmod_x.dll", L64), Verdict::OtherTarget(W32));
    }

    #[test]
    fn other_files_are_not_plugins() {
        for f in ["mp.dll", "ts_i386.so", "tsmod_readme.txt", "tsmod_x.so", "libtsmod_x_i386.so", "tsmod_"] {
            assert_eq!(classify(f, L32), Verdict::NotPlugin, "{f}");
        }
        assert_eq!(classify("tsmod__i386.so", L32), Verdict::BadName, "empty name");
        assert_eq!(classify("tsmod_a b_i386.so", L32), Verdict::BadName);
        assert_eq!(classify("tsmod_a.b_i386.so", L32), Verdict::BadName);
    }

    fn elf(class: u8, machine: u16) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b[..4].copy_from_slice(b"\x7fELF");
        b[4] = class;
        b[5] = 1;
        b[0x12..0x14].copy_from_slice(&machine.to_le_bytes());
        b
    }

    fn pe(machine: u16) -> Vec<u8> {
        let mut b = vec![0u8; 0x100];
        b[..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        b
    }

    #[test]
    fn headers_name_the_architecture() {
        assert_eq!(image_target(&elf(1, 3)), Some(L32));
        assert_eq!(image_target(&elf(2, 62)), Some(L64));
        assert_eq!(image_target(&elf(2, 183)), None, "aarch64");
        assert_eq!(image_target(&elf(1, 62)), None, "class and machine disagree");
        assert_eq!(image_target(&pe(0x14c)), Some(W32));
        assert_eq!(image_target(&pe(0x8664)), Some(W64));
        assert_eq!(image_target(&pe(0xaa64)), None, "arm64");
        assert_eq!(image_target(b"MZ"), None, "truncated");
        assert_eq!(image_target(b"hello world"), None);
        assert_eq!(image_target(&[]), None);
    }

    #[test]
    fn wrong_architecture_is_skipped() {
        let p = Path::new("dlls/tsmod_x_amd64.so");
        // Named for linux-amd64 but an i386 ELF inside.
        let o = decide("tsmod_x_amd64.so", p, L64, |_| Some(elf(1, 3))).unwrap();
        assert!(matches!(&o, Outcome::Skip(w) if w.contains("built for linux-i386")), "{o:?}");
        assert_eq!(decide("tsmod_x_amd64.so", p, L64, |_| Some(elf(2, 62))), Some(Outcome::Load("x".into())));
        // A PE image named as a Linux plugin, garbage, an unreadable file.
        assert!(matches!(decide("tsmod_x_amd64.so", p, L64, |_| Some(pe(0x8664))), Some(Outcome::Skip(_))));
        assert!(matches!(decide("tsmod_x_amd64.so", p, L64, |_| Some(b"not a library".to_vec())), Some(Outcome::Skip(_))));
        assert!(matches!(decide("tsmod_x_amd64.so", p, L64, |_| None), Some(Outcome::Skip(_))));
        // Another platform's name is skipped without reading it.
        let o = decide("tsmod_x.dll", p, L64, |_| panic!("not read")).unwrap();
        assert!(matches!(&o, Outcome::Skip(w) if w.contains("a win32 library")), "{o:?}");
        assert_eq!(decide("ts_amd64.so", p, L64, |_| panic!("not read")), None);
    }

    #[test]
    fn scan_sorts_and_filters_a_directory() {
        let dir = std::env::temp_dir().join(format!("tsmod-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tsmod_dir_amd64.so")).unwrap();
        let host_header = |t: Target| {
            if t.windows { pe(if t.x86_64 { 0x8664 } else { 0x14c }) } else { elf(1 + t.x86_64 as u8, if t.x86_64 { 62 } else { 3 }) }
        };
        let host = Target::HOST;
        let other = Target { windows: !host.windows, ..host };
        std::fs::write(dir.join(format!("tsmod_b{}", host.suffix())), host_header(host)).unwrap();
        std::fs::write(dir.join(format!("tsmod_a{}", host.suffix())), host_header(host)).unwrap();
        std::fs::write(dir.join(format!("tsmod_c{}", other.suffix())), host_header(other)).unwrap();
        std::fs::write(dir.join(format!("tsmod_junk{}", host.suffix())), b"junk").unwrap();
        std::fs::write(dir.join("readme.txt"), b"x").unwrap();
        let found = scan(&dir, host);
        let _ = std::fs::remove_dir_all(&dir);
        let summary: Vec<(String, bool)> = found.iter().map(|f| (f.file.clone(), matches!(f.outcome, Outcome::Load(_)))).collect();
        assert_eq!(
            summary,
            [
                (format!("tsmod_a{}", host.suffix()), true),
                (format!("tsmod_b{}", host.suffix()), true),
                (format!("tsmod_c{}", other.suffix()), false),
                (format!("tsmod_junk{}", host.suffix()), false),
            ]
        );
        assert!(scan(Path::new("/nonexistent/tsmod"), host).is_empty());
    }

    #[test]
    fn a_bad_descriptor_or_refusal_is_rejected() {
        let good = Plugin { ..Plugin::EMPTY };
        assert_eq!(check_loaded(OK, &good), Ok(()));
        assert!(check_loaded(1, &good).unwrap_err().contains("refused"));
        assert!(check_loaded(-2, &good).is_err());
        let small = Plugin { size: 8, ..Plugin::EMPTY };
        assert!(check_loaded(OK, &small).unwrap_err().contains("descriptor"));
        let old = Plugin { api_version: 2, ..Plugin::EMPTY };
        assert!(check_loaded(OK, &old).unwrap_err().contains("version 2"));
        // The server's blank descriptor isn't loadable as it is: the plugin must say its version.
        assert!(check_loaded(OK, &blank_descriptor()).is_err());
        assert_eq!(blank_descriptor().size as usize, size_of::<Plugin>());
    }
}
