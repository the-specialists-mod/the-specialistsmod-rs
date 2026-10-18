//! `cargo xtask plugin-approve <path>... [--root R | --game-dir D] [--platform P]`: writes an
//! approval for a local build into the game's `cl_plugins/approved.toml` (client-plugins.md §11,
//! issue #297). It's the choice a player makes in the start-up box, made at the terminal on the
//! developer's own machine, so e2e runs don't stop at a prompt.
//!
//! `<path>` is one of:
//! * a `.tspkg`: installed into `<gamedir>/cl_plugins/<id>/<version>/` (replacing that version),
//!   then approved; needs `--root` or `--game-dir`;
//! * an installed package directory, `.../cl_plugins/<id>/<version>/`;
//! * a hand-installed library, `.../cl_dlls/tsmod_<name><suffix>` (with its `tsmod_<name>.files`).
//!
//! Everything is hashed and verified exactly as the client does at start; a plugin the client
//! couldn't approve as it is (files that disagree with the package's manifest, a broken
//! signature, no library for the platform) is refused here too.

use plugin_pkg::{Candidate, Entry, Package, Platform, Scope, Store, Verdict};
use std::path::{Path, PathBuf};

/// What `approve` wrote.
pub struct Approved {
    pub entry: Entry,
    pub store: PathBuf,
}

/// The candidate `path` names, and the game directory it's in (`game_dir` if given).
fn candidate(path: &Path, game_dir: Option<&Path>, platform: Platform) -> Result<(Candidate, PathBuf), String> {
    let up = |p: &Path, n: usize| -> Option<PathBuf> {
        let mut p = std::path::absolute(p).ok()?;
        for _ in 0..n {
            p = p.parent()?.to_path_buf();
        }
        Some(p)
    };
    let need_game = || game_dir.map(Path::to_path_buf).ok_or_else(|| format!("{}: give --root or --game-dir", path.display()));
    if path.extension().is_some_and(|e| e == "tspkg") {
        let game = need_game()?;
        let mut pkg = Package::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (id, version) = (pkg.manifest().id.clone(), pkg.manifest().version.to_string());
        let dest = game.join("cl_plugins").join(id).join(version);
        let _ = std::fs::remove_dir_all(&dest);
        pkg.extract(&dest, platform).map_err(|e| format!("{}: {e}", path.display()))?;
        return Ok((Candidate::Package { dir: dest }, game));
    }
    if path.join("manifest.toml").is_file() {
        let game = match game_dir {
            Some(g) => g.to_path_buf(),
            None => up(path, 3).ok_or("no game directory above the package")?,
        };
        return Ok((Candidate::Package { dir: std::path::absolute(path).map_err(|e| e.to_string())? }, game));
    }
    let suffix = platform.library_suffix();
    let file = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
    let Some(name) = file.strip_prefix("tsmod_").and_then(|n| n.strip_suffix(suffix)) else {
        return Err(format!("{}: not a .tspkg, a package directory or a tsmod_<name>{suffix} library", path.display()));
    };
    let dir = up(path, 1).ok_or("no directory")?;
    let game = match game_dir {
        Some(g) => g.to_path_buf(),
        None => up(path, 2).ok_or("no game directory above cl_dlls")?,
    };
    Ok((Candidate::Manual { dir, name: name.to_string(), suffix: suffix.to_string() }, game))
}

/// Verifies `path` for `platform` and records an "Allow" for it (`edit` may change the entry,
/// e.g. its scope, before it's saved).
pub fn approve(path: &Path, game_dir: Option<&Path>, platform: Platform, edit: impl FnOnce(&mut Entry)) -> Result<Approved, String> {
    let (c, game) = candidate(path, game_dir, platform)?;
    let store_path = plugin_pkg::store_path(&game.join("cl_plugins"));
    let (mut store, _) = Store::load(&store_path).map_err(|e| format!("{}: {e}", store_path.display()))?;
    let v = plugin_pkg::verify(&c, platform, &store, &plugin_pkg::builtin_roots(), 0);
    if v.verdict == Verdict::Refuse {
        return Err(format!("{} can't be approved as it is: {}", v.id, plugin_pkg::change_lines(&v.changes, 10).join("; ")));
    }
    let mut entry = v.approval("manual", Scope::Always, Vec::new()).ok_or("can't be approved")?;
    edit(&mut entry);
    store.allow(entry.clone());
    store.save(&store_path).map_err(|e| format!("{}: {e}", store_path.display()))?;
    Ok(Approved { entry, store: store_path })
}

/// The command line (module docs).
pub fn main(mut args: Vec<String>) -> bool {
    let root = crate::take_opt(&mut args, "--root");
    let game_dir = crate::take_opt(&mut args, "--game-dir").map(PathBuf::from);
    let platform = crate::take_opt(&mut args, "--platform");
    let root = root.map(|r| crate::select_roots(&[r]).into_iter().next().expect("a root"));
    let game_dir = game_dir.or_else(|| root.as_ref().map(|r| r.path.join("ts")));
    let platform = match platform.or_else(|| root.as_ref().map(|r| r.target().platform.to_string())) {
        Some(p) => p.parse::<Platform>(),
        None => Platform::current().ok_or_else(|| "give --platform".to_string()),
    };
    let platform = match platform {
        Ok(p) => p,
        Err(e) => {
            eprintln!("plugin-approve: {e}");
            return false;
        }
    };
    if args.is_empty() {
        eprintln!(
            "usage: cargo xtask plugin-approve <tspkg|package dir|cl_dlls/tsmod_<name><suffix>>... [--root R | --game-dir D] [--platform P]"
        );
        return false;
    }
    let mut ok = true;
    for a in &args {
        match approve(Path::new(a), game_dir.as_deref(), platform, |_| {}) {
            Ok(a) => crate::say!(
                "== approved {} {} for {platform} ({} file(s), {}) in {}",
                a.entry.id,
                a.entry.version,
                a.entry.files.len(),
                format!("{:?}", a.entry.trust).to_lowercase(),
                a.store.display()
            ),
            Err(e) => {
                eprintln!("plugin-approve: {e}");
                ok = false;
            }
        }
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn approves_a_hand_installed_library_and_refuses_a_tampered_package() {
        let dir = std::env::temp_dir().join(format!("xtask-approve-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let game = dir.join("ts");
        fs::create_dir_all(game.join("cl_dlls")).unwrap();
        let lib = game.join("cl_dlls/tsmod_dev_amd64.so");
        fs::write(&lib, b"build").unwrap();
        let a = approve(&lib, None, Platform::LinuxX86_64, |_| {}).unwrap();
        assert_eq!(a.entry.id, "manual:dev");
        assert_eq!(a.store, game.join("cl_plugins/approved.toml"));
        let (store, _) = Store::load(&a.store).unwrap();
        let v = plugin_pkg::verify(
            &Candidate::Manual { dir: game.join("cl_dlls"), name: "dev".into(), suffix: "_amd64.so".into() },
            Platform::LinuxX86_64,
            &store,
            &[],
            1,
        );
        assert_eq!(v.verdict, Verdict::Load);
        assert!(approve(&game.join("cl_dlls/tsmod_dev.dll"), None, Platform::LinuxX86_64, |_| {}).is_err(), "another platform's name");

        // A package whose files disagree with its manifest.
        let pkg = game.join("cl_plugins/demo/1.0.0");
        fs::create_dir_all(pkg.join("files/lib")).unwrap();
        fs::write(pkg.join("files/lib/tsmod_demo.so"), b"lib").unwrap();
        let m = format!(
            "format = 1\nid = \"demo\"\nname = \"Demo\"\nversion = \"1.0.0\"\nclient_ext_api = 1\n\n[platforms]\nlinux-x86_64 = \"lib/tsmod_demo.so\"\n\n[[files]]\npath = \"lib/tsmod_demo.so\"\nsize = 3\nsha256 = \"{}\"\n",
            plugin_pkg::Sha256::of_bytes(b"lib")
        );
        fs::write(pkg.join("manifest.toml"), m).unwrap();
        assert_eq!(approve(&pkg, None, Platform::LinuxX86_64, |_| {}).unwrap().entry.id, "demo");
        fs::write(pkg.join("files/lib/tsmod_demo.so"), b"LIB").unwrap();
        let e = approve(&pkg, None, Platform::LinuxX86_64, |_| {}).err().unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert!(e.contains("can't be approved") && e.contains("changed: files/lib/tsmod_demo.so"), "{e}");
    }
}
