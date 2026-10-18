//! Finding tsmod-launcher (docs/implementation/client-plugins.md §13.5, "Finding the launcher"):
//! the `TSMOD_LAUNCHER*` environment it sets on the game, else `<gamedir>/launcher.toml`, which
//! it writes into every installation it manages:
//!
//! ```toml
//! format = 1
//! launcher = "/home/me/.local/bin/ts-launcher"   # the launcher's executable
//! launcher_version = "0.13.0"
//! instance = "ab12"                              # the installation id (`ts-launcher list`)
//! root = "/home/me/games/ts-xash"                # the installation root: the engine directory
//! ```
//!
//! A launcher is used only when its executable exists, is a file whose name starts with
//! `ts-launcher`, lies outside the game directory (where a server's downloads land), and is
//! executable (Unix); and, for `launcher.toml`, only when `root` is the directory the game runs in.

use super::plan::Launcher;
use std::path::{Path, PathBuf};

/// The launcher's environment variables, as read at start.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvVars {
    /// `TSMOD_LAUNCHER`: the launcher's executable.
    pub launcher: Option<String>,
    /// `TSMOD_LAUNCHER_INSTANCE`: the installation id.
    pub instance: Option<String>,
    /// `TSMOD_LAUNCHER_IPC`: the IPC endpoint, while the launcher runs.
    pub ipc: Option<String>,
}

impl EnvVars {
    pub fn from_env() -> EnvVars {
        let v = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
        EnvVars { launcher: v("TSMOD_LAUNCHER"), instance: v("TSMOD_LAUNCHER_INSTANCE"), ipc: v("TSMOD_LAUNCHER_IPC") }
    }
}

/// `launcher.toml`'s fields.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct LauncherToml {
    pub format: u32,
    pub launcher: String,
    #[serde(default)]
    pub launcher_version: Option<String>,
    pub instance: String,
    pub root: String,
}

/// The file's name in the game directory.
pub const FILE: &str = "launcher.toml";

pub fn parse_toml(text: &str) -> Result<LauncherToml, String> {
    let t: LauncherToml = toml::from_str(text).map_err(|e| e.to_string())?;
    if t.format != 1 {
        return Err(format!("format {} isn't 1", t.format));
    }
    valid_instance(&t.instance)?;
    Ok(t)
}

/// An installation id: `[A-Za-z0-9_.-]`, 1 to 64 bytes.
fn valid_instance(id: &str) -> Result<(), String> {
    if !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)) {
        Ok(())
    } else {
        Err(format!("instance {id:?} isn't an installation id"))
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether `exe` may be started as the launcher for the game directory `game_dir`.
pub fn check_exe(exe: &Path, game_dir: &Path) -> Result<PathBuf, String> {
    let exe = exe.canonicalize().map_err(|e| format!("{}: {e}", exe.display()))?;
    let meta = std::fs::metadata(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    if !meta.is_file() {
        return Err(format!("{} isn't a file", exe.display()));
    }
    let name = exe.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if !name.starts_with("ts-launcher") {
        return Err(format!("{} isn't named ts-launcher*", exe.display()));
    }
    if let Ok(g) = game_dir.canonicalize()
        && exe.starts_with(&g)
    {
        return Err(format!("{} is inside the game directory", exe.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(format!("{} isn't executable", exe.display()));
        }
    }
    Ok(exe)
}

/// The launcher to start with `relaunch` arguments (case 2): the environment's, else
/// `launcher.toml`'s. `root` is the directory the game runs in, `game_dir` its game directory.
/// The second value lists why a candidate was ignored, for the console.
pub fn find(env: &EnvVars, root: &Path, game_dir: &Path, toml_text: Option<&str>) -> (Option<Launcher>, Vec<String>) {
    let mut notes = Vec::new();
    if let Some(exe) = &env.launcher {
        match (check_exe(Path::new(exe), game_dir), env.instance.as_deref().map(valid_instance)) {
            (Ok(exe), Some(Ok(()))) => return (Some(Launcher { exe, instance: env.instance.clone().unwrap() }), notes),
            (Err(e), _) => notes.push(format!("TSMOD_LAUNCHER ignored: {e}")),
            (Ok(_), Some(Err(e))) => notes.push(format!("TSMOD_LAUNCHER ignored: {e}")),
            (Ok(_), None) => notes.push("TSMOD_LAUNCHER ignored: TSMOD_LAUNCHER_INSTANCE isn't set".into()),
        }
    }
    let Some(text) = toml_text else { return (None, notes) };
    match parse_toml(text) {
        Err(e) => notes.push(format!("{FILE} ignored: {e}")),
        Ok(t) if !same_dir(Path::new(&t.root), root) => {
            notes.push(format!("{FILE} ignored: its root {:?} isn't this installation ({})", t.root, root.display()))
        }
        Ok(t) => match check_exe(Path::new(&t.launcher), game_dir) {
            Ok(exe) => return (Some(Launcher { exe, instance: t.instance }), notes),
            Err(e) => notes.push(format!("{FILE} ignored: {e}")),
        },
    }
    (None, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    /// root/ with ts/ and an executable root/bin/ts-launcher.
    fn install(tag: &str) -> (Dir, PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("tsrs-launcher-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let game = root.join("ts");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let exe = root.join("bin").join(if cfg!(windows) { "ts-launcher.exe" } else { "ts-launcher" });
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (Dir(root.clone()), root, game, exe)
    }
    fn toml_for(exe: &Path, root: &Path) -> String {
        format!(
            "format = 1\nlauncher = {:?}\nlauncher_version = \"0.13.0\"\ninstance = \"ab12\"\nroot = {:?}\n",
            exe.display().to_string(),
            root.display().to_string()
        )
    }

    #[test]
    fn launcher_toml_discovery() {
        let (_d, root, game, exe) = install("toml");
        let text = toml_for(&exe, &root);
        let (l, notes) = find(&EnvVars::default(), &root, &game, Some(&text));
        assert_eq!(l, Some(Launcher { exe: exe.canonicalize().unwrap(), instance: "ab12".into() }), "{notes:?}");
        // Another installation's file is ignored.
        let other = toml_for(&exe, &game);
        let (l, notes) = find(&EnvVars::default(), &root, &game, Some(&other));
        assert!(l.is_none() && notes[0].contains("isn't this installation"), "{notes:?}");
        // A missing executable, a wrong name, one inside the game directory.
        let gone = toml_for(&root.join("bin/ts-launcher-gone"), &root);
        assert!(find(&EnvVars::default(), &root, &game, Some(&gone)).0.is_none());
        let sh = root.join("bin/sh");
        std::fs::copy(&exe, &sh).unwrap();
        let (l, notes) = find(&EnvVars::default(), &root, &game, Some(&toml_for(&sh, &root)));
        assert!(l.is_none() && notes[0].contains("ts-launcher"), "{notes:?}");
        let planted = game.join("ts-launcher");
        std::fs::copy(&exe, &planted).unwrap();
        let (l, notes) = find(&EnvVars::default(), &root, &game, Some(&toml_for(&planted, &root)));
        assert!(l.is_none() && notes[0].contains("inside the game directory"), "{notes:?}");
        // Bad files.
        for bad in [
            "",
            "format = 2\nlauncher = \"x\"\ninstance = \"a\"\nroot = \"/\"\n",
            "format = 1\nlauncher = \"x\"\ninstance = \"a b\"\nroot = \"/\"\n",
        ] {
            assert!(find(&EnvVars::default(), &root, &game, Some(bad)).0.is_none(), "{bad:?}");
        }
        assert!(find(&EnvVars::default(), &root, &game, None).0.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn not_executable() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, root, game, exe) = install("noexec");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).unwrap();
        let (l, notes) = find(&EnvVars::default(), &root, &game, Some(&toml_for(&exe, &root)));
        assert!(l.is_none() && notes[0].contains("executable"), "{notes:?}");
    }

    #[test]
    fn environment_first() {
        let (_d, root, game, exe) = install("env");
        let env = EnvVars { launcher: Some(exe.display().to_string()), instance: Some("env1".into()), ipc: None };
        let (l, _) = find(&env, &root, &game, Some(&toml_for(&exe, &root)));
        assert_eq!(l.unwrap().instance, "env1");
        // Without an instance id the environment is ignored and the file is used.
        let env = EnvVars { instance: None, ..env };
        let (l, notes) = find(&env, &root, &game, Some(&toml_for(&exe, &root)));
        assert_eq!(l.unwrap().instance, "ab12");
        assert!(notes[0].contains("TSMOD_LAUNCHER_INSTANCE"), "{notes:?}");
    }
}
