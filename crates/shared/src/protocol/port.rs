//! Port additions that tell the two sides apart from TS 3.0's own libraries, on channels TS 3.0
//! tolerates (issue #100, `docs/implementation/gameplay-plugins.md` §2):
//!
//! * The port client announces itself and its version in its userinfo ([`USERINFO_KEY`], a
//!   `FCVAR_USERINFO` cvar the client registers). TS 3.0's client has no such cvar, so the key
//!   is missing from its userinfo. The server reads it in `ClientConnect`
//!   ([`parse_userinfo_version`]: `3.0.2`, or the [`numeric_version`] a client sets first).
//! * The port server sends its version to the HUD as `SrvSett` keys [`SRVSETT_VERSION_KEYS`]
//!   (major, minor, patch) after key 4. TS 3.0's `SrvSett` handler (client.dll 0x1004b840)
//!   switches on keys 0..3 and only sets the HUD's redraw flag for any other key.

/// The version of the library this is built into: the workspace's (`Cargo.toml`), which the
/// release tags follow (`v3.0.2`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// [`VERSION`] for the engine (the client's cvar default).
pub const VERSION_C: &std::ffi::CStr = match std::ffi::CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
    Ok(c) => c,
    Err(_) => panic!("the package version has a NUL"),
};

/// The userinfo key (and client cvar) that carries the port client's version. The leading `_`
/// keeps it out of the userinfo the engines send to the other clients.
pub const USERINFO_KEY: &str = "_tsrs";
/// [`USERINFO_KEY`] for the engine's info-key calls.
pub const USERINFO_KEY_C: &std::ffi::CStr = c"_tsrs";

/// The `SrvSett` keys that carry the server's version: major, minor, patch.
pub const SRVSETT_VERSION_KEYS: [u8; 3] = [5, 6, 7];

/// The first port client version that hooks `TSPkg` ([`super::tspkg`]): the release after
/// 3.0.8, the first with the hook. The server sends `TSPkg` only to clients whose `_tsrs` version
/// is at least this (`docs/implementation/client-plugins.md` §12): an older port client carries
/// `_tsrs` but doesn't hook the message, and a GoldSrc client can fail on one it hasn't hooked.
pub const TSPKG_SINCE: [i16; 3] = [3, 0, 9];

/// Whether a client at `version` hooks `TSPkg`.
pub fn hooks_tspkg(version: [i16; 3]) -> bool {
    version >= TSPKG_SINCE
}

/// `major.minor.patch` as three numbers, each fitting `SrvSett`'s `short`; anything after the
/// patch number (`-rc1`, `+dirty`) is dropped. `None` if it isn't such a version.
pub fn parse_version(v: &str) -> Option<[i16; 3]> {
    let mut parts = v.trim().splitn(3, '.');
    let mut out = [0i16; 3];
    for (i, slot) in out.iter_mut().enumerate() {
        let p = parts.next()?;
        let p = if i == 2 { p.split(|c: char| !c.is_ascii_digit()).next()? } else { p };
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = p.parse().ok()?;
    }
    Some(out)
}

/// The version as one number, `major * 1_000_000 + minor * 1_000 + patch` (`3000002` for
/// 3.0.2), for the client's first set of [`USERINFO_KEY`]: the documented client API sets cvars
/// only from a float (`Cvar_SetValue`, which writes a whole value as `%d`), and GoldSrc puts the
/// key in the userinfo only once it is set. The string version follows by a queued command, which
/// runs after the command line's `+connect`, so a client launched with `+connect` first joins with
/// this. `None` when the number isn't exact in an `f32` (major above 15: 2^24 is 16 777 216), the major is 0 (it would
/// read as the old placeholder `1`), or a part is out of range.
pub fn numeric_version(v: [i16; 3]) -> Option<f32> {
    let [major, minor, patch] = v.map(i32::from);
    if !(1..=15).contains(&major) || !(0..1000).contains(&minor) || !(0..1000).contains(&patch) {
        return None;
    }
    Some((major * 1_000_000 + minor * 1_000 + patch) as f32)
}

/// A [`USERINFO_KEY`] value as a version: `major.minor.patch` ([`parse_version`]) or the
/// [`numeric_version`] form. `None` for anything else, such as the `1` that clients from before
/// the numeric form set first (issue #196).
pub fn parse_userinfo_version(v: &str) -> Option<[i16; 3]> {
    let v = v.trim();
    if v.contains('.') {
        return parse_version(v);
    }
    let n: u32 = v.parse().ok().filter(|_| v.bytes().all(|b| b.is_ascii_digit()))?;
    if !(1_000_000..16_000_000).contains(&n) {
        return None;
    }
    Some([(n / 1_000_000) as i16, (n / 1_000 % 1_000) as i16, (n % 1_000) as i16])
}

/// This library's version as `SrvSett` values.
pub fn version_numbers() -> [i16; 3] {
    parse_version(VERSION).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("3.0.2"), Some([3, 0, 2]));
        assert_eq!(parse_version(" 10.20.30-rc1 "), Some([10, 20, 30]));
        assert_eq!(parse_version("3.0"), None);
        assert_eq!(parse_version("3.x.1"), None);
        assert_eq!(parse_version("99999.0.0"), None, "too big for a short");
        assert_eq!(parse_version(""), None);
        assert!(parse_version(VERSION).is_some(), "the workspace version is numeric");
    }

    #[test]
    fn userinfo_versions() {
        assert_eq!(numeric_version([3, 0, 2]), Some(3_000_002.0));
        assert_eq!(numeric_version([15, 999, 999]), Some(15_999_999.0));
        assert_eq!(numeric_version([16, 0, 0]), None, "not exact in an f32");
        assert_eq!(numeric_version([3, 1000, 0]), None);
        assert_eq!(numeric_version([0, 0, 1]), None, "would be the placeholder 1");
        assert_eq!(parse_userinfo_version("3.0.2"), Some([3, 0, 2]));
        assert_eq!(parse_userinfo_version("3000002"), Some([3, 0, 2]));
        assert_eq!(parse_userinfo_version("3010020"), Some([3, 10, 20]));
        assert_eq!(parse_userinfo_version("1"), None, "the old placeholder");
        for bad in ["", "+3000002", "-3000002", "3000002.5", "999999", "16000000", "abc"] {
            assert_eq!(parse_userinfo_version(bad), None, "{bad:?}");
        }
        // What the engines write for the client's float: `%d` of the value, exact for every
        // version the numeric form allows.
        for v in [[1, 0, 0], [3, 0, 2], [3, 999, 999], [15, 999, 999]] {
            let text = format!("{}", numeric_version(v).unwrap() as i32);
            assert_eq!(parse_userinfo_version(&text), Some(v), "{text}");
        }
        assert_eq!(
            numeric_version(version_numbers()).and_then(|f| parse_userinfo_version(&(f as i32).to_string())),
            Some(version_numbers())
        );
    }

    #[test]
    fn tspkg_since() {
        assert!(!hooks_tspkg([3, 0, 8]), "3.0.8 shipped without the hook");
        assert!(hooks_tspkg([3, 0, 9]) && hooks_tspkg([3, 1, 0]) && hooks_tspkg([4, 0, 0]));
        assert!(!hooks_tspkg([2, 9, 99]));
    }

    #[test]
    fn version_keys_follow_the_ruleset_key() {
        // TS 3.0 uses 0..3 and the gameplay ruleset 4.
        assert!(SRVSETT_VERSION_KEYS.iter().all(|&k| k > 4));
    }
}
