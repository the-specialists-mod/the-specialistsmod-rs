//! The offer flow's decisions (client-plugins.md §6.3 steps 1–3, §6.4, §7.3), engine-free so the
//! unit tests run them: what an offer may be fetched over ([`transport`]), what the fetched
//! manifest means ([`assess`]), and the per-connection limits ([`Connection`]).

use plugin_pkg::{Manifest, Platform, PublicKey, Scope, Sha256, Signer, State, Store, TrustInfo, UpdateSummary, Version, VersionChange};
use shared::protocol::tspkg::{Offer, State as Reply};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// At most this many offers per server per connection reach the player (§7.3).
pub const MAX_OFFERS: usize = 4;

/// The files fetched before the package, beside it (§3.1): `<base>.manifest.toml`,
/// `<base>.manifest.sig`, `<base>.publisher.cert`.
pub const MANIFEST_SUFFIX: &str = ".manifest.toml";
pub const SIGNATURE_SUFFIX: &str = ".manifest.sig";
pub const CERT_SUFFIX: &str = ".publisher.cert";

/// How an offer's files may be fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transport {
    /// The package's URL without `.tspkg`: the siblings' prefix.
    pub base: String,
    /// Plain `http://` is allowed (a FastDL offer, §6.3 "HTTP"); otherwise HTTPS only, redirects
    /// included.
    pub plain_http: bool,
}

/// Checks an offer's URL (§6.3 "HTTP"): HTTPS, or plain HTTP for a `fastdl` offer, ending with
/// `.tspkg`.
pub fn transport(o: &Offer) -> Result<Transport, String> {
    let lower = o.url.to_ascii_lowercase();
    let plain = lower.starts_with("http://");
    if !plain && !lower.starts_with("https://") {
        return Err("the URL isn't http(s)".into());
    }
    if plain && !o.fastdl() {
        return Err("plain http is only for the server's FastDL".into());
    }
    let base = o.url.strip_suffix(".tspkg").ok_or("the URL isn't a .tspkg")?;
    if base.contains(['?', '#']) {
        return Err("the URL has a query".into());
    }
    Ok(Transport { base: base.to_owned(), plain_http: plain })
}

/// What the client fetched before the package.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fetched {
    pub manifest: Vec<u8>,
    pub signature: Option<Vec<u8>>,
    pub cert: Option<Vec<u8>>,
}

/// What [`assess`] checks against.
pub struct Ctx<'a> {
    pub platform: Platform,
    pub port: &'a Version,
    /// The client extension API version (§9).
    pub api: u32,
    /// The server's address, as the net API names it (the `scope = "server"` key).
    pub server: &'a str,
    /// `cl_plugins_allow_unsigned`.
    pub allow_unsigned: bool,
    pub roots: &'a [PublicKey],
    pub store: &'a Store,
    /// `<gamedir>/cl_plugins`.
    pub cl_plugins: &'a Path,
}

/// A manifest whose signature checked and that is the one offered.
#[derive(Clone, Debug)]
pub struct Checked {
    pub manifest: Manifest,
    pub bytes: Vec<u8>,
    pub sha256: Sha256,
    pub signer: Signer,
    pub trust: TrustInfo,
}

impl Checked {
    pub fn key(&self) -> Option<PublicKey> {
        self.signer.key()
    }
}

/// An approved earlier version (or build) of the offered plugin: the update card (§6.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub old: Version,
    pub change: VersionChange,
    /// From the installed copy's manifest; `None` when it can't be read.
    pub summary: Option<UpdateSummary>,
    /// The approval's publisher key isn't the offer's (a different plugin as far as trust goes).
    pub key_changed: bool,
}

/// What the offer becomes once its manifest is in.
#[derive(Debug)]
pub enum Assessment {
    /// Answered without a card: `failed`, `unsupported` or `blocked`, with a reason.
    Answer(Reply, String),
    /// The same id, version and manifest is approved and applies here (§6.3 step 1 `have`).
    Have,
    /// Show a card.
    Card(Box<CardInfo>),
}

#[derive(Debug)]
pub struct CardInfo {
    pub checked: Checked,
    pub update: Option<Update>,
    /// The same manifest is installed and approved, but for other servers: "Allow" needs no
    /// download.
    pub installed: Option<PathBuf>,
}

/// `cl_plugins/<id>/<version>/`.
pub fn version_dir(cl_plugins: &Path, id: &str, version: &Version) -> PathBuf {
    cl_plugins.join(id).join(version.to_string())
}

/// Checks the fetched manifest and decides (§6.3 steps 1–2, §6.4).
pub fn assess(o: &Offer, t: &Transport, f: &Fetched, cx: &Ctx) -> Assessment {
    use Assessment::Answer;
    let manifest = match Manifest::parse(&f.manifest) {
        Ok(m) => m,
        Err(e) => return Answer(Reply::Failed, format!("manifest: {e}")),
    };
    if manifest.id != o.id || manifest.version.to_string() != o.version {
        return Answer(Reply::Failed, format!("the manifest is {} {}, not the offer", manifest.id, manifest.version));
    }
    let signer = match plugin_pkg::check_signature(&manifest, &f.manifest, f.signature.as_deref(), f.cert.as_deref(), cx.roots) {
        Ok(s) => s,
        Err(e) => return Answer(Reply::Failed, format!("signature: {e}")),
    };
    if signer == Signer::Unsigned {
        if t.plain_http {
            return Answer(Reply::Failed, "unsigned, over plain http".into());
        }
        if !cx.allow_unsigned {
            return Answer(Reply::Failed, "unsigned (cl_plugins_allow_unsigned is 0)".into());
        }
    }
    if cx.store.is_blocked(&o.id, signer.key(), Some(cx.server)) {
        return Answer(Reply::Blocked, String::new());
    }
    if !manifest.supports(cx.platform) {
        return Answer(Reply::Unsupported, format!("no library for {}", cx.platform));
    }
    if !manifest.port_requirement_met(cx.port) {
        let req = manifest.requires.port.as_ref().map(ToString::to_string).unwrap_or_default();
        return Answer(Reply::Unsupported, format!("needs port {req}"));
    }
    if manifest.client_ext_api != cx.api {
        return Answer(Reply::Failed, "api".into());
    }
    let sha256 = Sha256::of_bytes(&f.manifest);
    let trust = TrustInfo::assess(&signer, manifest.publisher.as_ref().map(|p| p.name.as_str()), cx.store);
    let version = manifest.version.clone();
    let checked = Checked { manifest, bytes: f.manifest.clone(), sha256, signer, trust };
    let allowed = |e: &&plugin_pkg::Entry| e.state == State::Allowed && e.platform == cx.platform;
    let dir = version_dir(cx.cl_plugins, &o.id, &version);
    let same = cx.store.entries_for(&o.id).filter(allowed).find(|e| e.version == version && e.manifest_sha256 == Some(sha256));
    if let Some(e) = same {
        if e.applies_to(Some(cx.server)) {
            return Assessment::Have;
        }
        let installed = dir.join(plugin_pkg::package::MANIFEST_FILE).is_file().then_some(dir);
        return Assessment::Card(Box::new(CardInfo { checked, update: None, installed }));
    }
    let update = cx.store.entries_for(&o.id).find(allowed).map(|e| {
        let old_manifest = std::fs::read(version_dir(cx.cl_plugins, &o.id, &e.version).join(plugin_pkg::package::MANIFEST_FILE))
            .ok()
            .and_then(|b| Manifest::parse(&b).ok());
        Update {
            old: e.version.clone(),
            change: VersionChange::between(&e.version, &version),
            summary: old_manifest.map(|m| UpdateSummary::between(&m, &checked.manifest, cx.platform)),
            key_changed: e.publisher_key != checked.key(),
        }
    });
    Assessment::Card(Box::new(CardInfo { checked, update, installed: None }))
}

/// The store entry a "Never" click writes (§7.2): this server only, or (Shift) everywhere.
pub fn blocking(c: &Checked, platform: Platform, server: &str, everywhere: bool) -> plugin_pkg::Entry {
    plugin_pkg::Entry {
        id: c.manifest.id.clone(),
        version: c.manifest.version.clone(),
        state: State::Blocked,
        scope: if everywhere { Scope::Always } else { Scope::Server },
        servers: if everywhere { Vec::new() } else { vec![server.to_owned()] },
        source: source(server),
        publisher_key: c.key(),
        trust: c.trust.trust,
        approved_at: plugin_pkg::time::now_rfc3339(),
        manifest_sha256: Some(c.sha256),
        platform,
        files: Default::default(),
    }
}

/// The servers an "Allow on this server only" records: this one, plus those an earlier approval
/// of exactly these files (same version and manifest) already listed, so allowing it on a second
/// server doesn't revoke it on the first. Any other approval is replaced whole (§5.1).
pub fn servers_with(store: &Store, v: &plugin_pkg::Verification, scope: Scope, mut servers: Vec<String>) -> Vec<String> {
    if scope != Scope::Server {
        return servers;
    }
    let earlier = store.entries_for(&v.id).find(|e| {
        e.state == State::Allowed
            && e.scope == Scope::Server
            && e.platform == v.platform
            && Some(&e.version) == v.version.as_ref()
            && e.manifest_sha256 == v.manifest_sha256
    });
    for s in earlier.map(|e| e.servers.clone()).unwrap_or_default() {
        if !servers.contains(&s) {
            servers.insert(0, s);
        }
    }
    servers
}

/// The `source` an approval from this server records (§5.1).
pub fn source(server: &str) -> String {
    format!("server {server}")
}

/// What one connection to one server has seen (§7.3 "No flooding").
#[derive(Clone, Debug, Default)]
pub struct Connection {
    /// The ids offered so far, in order; the first [`MAX_OFFERS`] are handled.
    pub offered: Vec<String>,
    /// Ids the player refused this connection: not shown again.
    pub refused: BTreeSet<String>,
    /// `tspkg list` was sent this map (after the first `TSPkg`, or the server's version, §13.2).
    pub listed: bool,
}

/// What to do with an incoming offer, before anything is fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admit {
    /// Fetch its manifest.
    Fetch,
    /// Refused earlier this connection: answer `denied` again, no card.
    Refused,
    /// Already being handled (the same offer again, after a map change).
    Busy,
    /// Over [`MAX_OFFERS`] this connection: ignored.
    Flood,
}

impl Connection {
    /// Decides about an offer of `id`. `busy`: an offer for `id` is still being handled.
    pub fn admit(&mut self, id: &str, busy: bool) -> Admit {
        if self.refused.contains(id) {
            return Admit::Refused;
        }
        if busy {
            return Admit::Busy;
        }
        if !self.offered.iter().any(|o| o == id) {
            if self.offered.len() >= MAX_OFFERS {
                return Admit::Flood;
            }
            self.offered.push(id.to_owned());
        }
        Admit::Fetch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_pkg::{SigningKey, Trust};

    const P: Platform = Platform::LinuxX86_64;

    fn offer(id: &str, url: &str, flags: u8) -> Offer {
        Offer { id: id.into(), version: "1.0.0".into(), size: 1, sha256: [0; 32], flags, url: url.into(), note: String::new() }
    }

    fn manifest(id: &str, version: &str, key: Option<&SigningKey>, extra: &str) -> Vec<u8> {
        let lib = b"lib";
        let publisher = key.map_or(String::new(), |k| format!("[publisher]\nname = \"Pub\"\nkey = \"{}\"\n\n", PublicKey::of(k)));
        format!(
            "format = 1\nid = \"{id}\"\nname = \"Plugin\"\nversion = \"{version}\"\nclient_ext_api = 1\n{extra}\n{publisher}[platforms]\nlinux-x86_64 = \"lib/x.so\"\n\n[[files]]\npath = \"lib/x.so\"\nsize = 3\nsha256 = \"{}\"\n",
            Sha256::of_bytes(lib)
        )
        .into_bytes()
    }

    fn fetched(m: Vec<u8>, key: Option<&SigningKey>) -> Fetched {
        let signature = key.map(|k| plugin_pkg::sign_manifest(k, &m).to_vec());
        Fetched { manifest: m, signature, cert: None }
    }

    struct Env {
        store: Store,
        dir: tempfile::TempDir,
        port: Version,
    }

    impl Env {
        fn new() -> Env {
            Env { store: Store::new(), dir: tempfile::tempdir().unwrap(), port: Version::new(3, 0, 8) }
        }
        fn cx(&self, allow_unsigned: bool) -> Ctx<'_> {
            Ctx {
                platform: P,
                port: &self.port,
                api: 1,
                server: "203.0.113.5:27015",
                allow_unsigned,
                roots: &[],
                store: &self.store,
                cl_plugins: self.dir.path(),
            }
        }
        fn approve(&mut self, id: &str, version: &str, m: &[u8], key: Option<&SigningKey>, scope: Scope, servers: &[&str]) {
            self.store.allow(plugin_pkg::Entry {
                id: id.into(),
                version: version.parse().unwrap(),
                state: State::Allowed,
                scope,
                servers: servers.iter().map(|s| s.to_string()).collect(),
                source: "manual".into(),
                publisher_key: key.map(PublicKey::of),
                trust: Trust::New,
                approved_at: "2026-10-17T10:42:00Z".into(),
                manifest_sha256: Some(Sha256::of_bytes(m)),
                platform: P,
                files: Default::default(),
            });
        }
    }

    fn answer(a: Assessment) -> (Reply, String) {
        match a {
            Assessment::Answer(s, r) => (s, r),
            other => panic!("{other:?}"),
        }
    }

    fn card(a: Assessment) -> CardInfo {
        match a {
            Assessment::Card(c) => *c,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn transports() {
        let t = transport(&offer("a", "https://h/p/a-1.0.0.tspkg", 0)).unwrap();
        assert_eq!(t, Transport { base: "https://h/p/a-1.0.0".into(), plain_http: false });
        assert!(transport(&offer("a", "http://h/a.tspkg", 0)).unwrap_err().contains("FastDL"));
        assert!(transport(&offer("a", "http://h/a.tspkg", 2)).unwrap().plain_http, "fastdl may be plain http");
        assert!(transport(&offer("a", "ftp://h/a.tspkg", 2)).is_err());
        assert!(transport(&offer("a", "https://h/a.zip", 0)).is_err());
        assert!(transport(&offer("a", "https://h/a.tspkg?x=1.tspkg", 0)).is_err());
    }

    #[test]
    fn signed_new_offers_get_a_card() {
        let env = Env::new();
        let k = SigningKey::from_bytes(&[7; 32]);
        let o = offer("demo", "https://h/demo-1.0.0.tspkg", 0);
        let t = transport(&o).unwrap();
        let c = card(assess(&o, &t, &fetched(manifest("demo", "1.0.0", Some(&k), ""), Some(&k)), &env.cx(false)));
        assert_eq!(c.checked.trust.trust, Trust::New);
        assert!(c.update.is_none() && c.installed.is_none());
    }

    #[test]
    fn refusals_before_the_card() {
        let mut env = Env::new();
        let k = SigningKey::from_bytes(&[7; 32]);
        let o = offer("demo", "https://h/demo-1.0.0.tspkg", 0);
        let t = transport(&o).unwrap();
        let ok = || fetched(manifest("demo", "1.0.0", Some(&k), ""), Some(&k));
        // A signature that doesn't check.
        let mut bad = ok();
        bad.manifest.extend_from_slice(b"\n");
        assert_eq!(answer(assess(&o, &t, &bad, &env.cx(false))).0, Reply::Failed);
        // Another id or version than offered.
        let other = fetched(manifest("demo", "1.0.1", Some(&k), ""), Some(&k));
        assert!(answer(assess(&o, &t, &other, &env.cx(false))).1.contains("not the offer"));
        // Unsigned: refused unless allowed, and always over plain http.
        let unsigned = fetched(manifest("demo", "1.0.0", None, ""), None);
        assert!(answer(assess(&o, &t, &unsigned, &env.cx(false))).1.contains("cl_plugins_allow_unsigned"));
        assert_eq!(card(assess(&o, &t, &unsigned, &env.cx(true))).checked.trust.trust, Trust::Unsigned);
        let plain = offer("demo", "http://h/demo-1.0.0.tspkg", 2);
        let pt = transport(&plain).unwrap();
        assert!(answer(assess(&plain, &pt, &unsigned, &env.cx(true))).1.contains("plain http"));
        assert!(matches!(assess(&plain, &pt, &ok(), &env.cx(false)), Assessment::Card(_)), "signed over FastDL is fine");
        // Unsupported platform, unmet port, another API.
        let m = String::from_utf8(manifest("demo", "1.0.0", Some(&k), "")).unwrap().replace("linux-x86_64 = ", "windows-i686 = ");
        let m = m.replace("path = \"lib/x.so\"", "path = \"lib/x.so\"\nplatform = \"windows-i686\"");
        assert_eq!(answer(assess(&o, &t, &fetched(m.into_bytes(), Some(&k)), &env.cx(false))).0, Reply::Unsupported);
        let port = fetched(manifest("demo", "1.0.0", Some(&k), "\n[requires]\nport = \">=9.0.0\"\n"), Some(&k));
        assert_eq!(answer(assess(&o, &t, &port, &env.cx(false))), (Reply::Unsupported, "needs port >=9.0.0".into()));
        let api = String::from_utf8(manifest("demo", "1.0.0", Some(&k), "")).unwrap().replace("client_ext_api = 1", "client_ext_api = 2");
        assert_eq!(answer(assess(&o, &t, &fetched(api.into_bytes(), Some(&k)), &env.cx(false))), (Reply::Failed, "api".into()));
        // Blocked from this server (by key).
        let c = card(assess(&o, &t, &ok(), &env.cx(false)));
        env.store.block(blocking(&c.checked, P, "203.0.113.5:27015", false));
        assert_eq!(answer(assess(&o, &t, &ok(), &env.cx(false))).0, Reply::Blocked);
        let other_server = |env: &Env| assess(&o, &t, &ok(), &Ctx { server: "198.51.100.1:27015", ..env.cx(false) });
        assert!(matches!(other_server(&env), Assessment::Card(_)), "blocked on that server only");
        env.store.block(blocking(&c.checked, P, "x", true));
        assert_eq!(answer(other_server(&env)).0, Reply::Blocked, "Never everywhere");
    }

    #[test]
    fn have_updates_and_other_servers() {
        let mut env = Env::new();
        let k = SigningKey::from_bytes(&[7; 32]);
        let o = offer("demo", "https://h/demo-1.0.0.tspkg", 0);
        let t = transport(&o).unwrap();
        let m = manifest("demo", "1.0.0", Some(&k), "");
        // Approved elsewhere only, installed: a card that needs no download.
        env.approve("demo", "1.0.0", &m, Some(&k), Scope::Server, &["198.51.100.1:27015"]);
        let dir = version_dir(env.dir.path(), "demo", &"1.0.0".parse().unwrap());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.toml"), &m).unwrap();
        let c = card(assess(&o, &t, &fetched(m.clone(), Some(&k)), &env.cx(false)));
        assert_eq!(c.installed.as_deref(), Some(dir.as_path()));
        assert_eq!(c.checked.trust.trust, Trust::Known);
        // Approved here (or everywhere): have.
        env.approve("demo", "1.0.0", &m, Some(&k), Scope::Always, &[]);
        assert!(matches!(assess(&o, &t, &fetched(m.clone(), Some(&k)), &env.cx(false)), Assessment::Have));
        // A newer version: the update card, with the installed manifest's summary.
        let o2 = Offer { version: "1.1.0".into(), url: "https://h/demo-1.1.0.tspkg".into(), ..o.clone() };
        let m2 = manifest("demo", "1.1.0", Some(&k), "\n[declares]\nnetwork = true\n");
        let c = card(assess(&o2, &transport(&o2).unwrap(), &fetched(m2, Some(&k)), &env.cx(false)));
        let u = c.update.unwrap();
        assert_eq!((u.old.to_string(), u.change, u.key_changed), ("1.0.0".into(), VersionChange::Upgrade, false));
        assert_eq!(u.summary.unwrap().declares_changed, ["network"]);
        // An older one: a downgrade, and another key is flagged.
        let k2 = SigningKey::from_bytes(&[8; 32]);
        let o3 = Offer { version: "0.9.0".into(), url: "https://h/demo-0.9.0.tspkg".into(), ..o.clone() };
        let c = card(assess(&o3, &transport(&o3).unwrap(), &fetched(manifest("demo", "0.9.0", Some(&k2), ""), Some(&k2)), &env.cx(false)));
        let u = c.update.unwrap();
        assert!(u.change.is_downgrade() && u.key_changed);
        // The same version rebuilt (another manifest): an update card, not have.
        let rebuilt = manifest("demo", "1.0.0", Some(&k), "description = \"rebuilt\"\n");
        let c = card(assess(&o, &t, &fetched(rebuilt, Some(&k)), &env.cx(false)));
        assert_eq!(c.update.unwrap().change, VersionChange::Same);
    }

    #[test]
    fn allowing_on_a_second_server_keeps_the_first() {
        let (pkg, _, _) = crate::plugins::offers::fetch::tests::package();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("demo/1.0.0");
        plugin_pkg::Package::from_reader(std::io::Cursor::new(pkg)).unwrap().extract(&dest, P).unwrap();
        let mut store = Store::new();
        let v = plugin_pkg::verify(&plugin_pkg::Candidate::Package { dir: dest.clone() }, P, &store, &[], 1);
        let a = vec!["198.51.100.1:27015".to_string()];
        assert_eq!(servers_with(&store, &v, Scope::Server, a.clone()), a);
        store.allow(v.approval("server x", Scope::Server, a.clone()).unwrap());
        let b = "203.0.113.5:27015".to_string();
        assert_eq!(servers_with(&store, &v, Scope::Server, vec![b.clone()]), [a[0].clone(), b.clone()]);
        assert!(servers_with(&store, &v, Scope::Always, Vec::new()).is_empty());
        // Another build of the same version starts over.
        let mut other = v.clone();
        other.manifest_sha256 = Some(Sha256::of_bytes(b"other"));
        assert_eq!(servers_with(&store, &other, Scope::Server, vec![b.clone()]), [b]);
    }

    #[test]
    fn connection_limits() {
        let mut c = Connection::default();
        for id in ["a", "b", "c", "d"] {
            assert_eq!(c.admit(id, false), Admit::Fetch);
        }
        assert_eq!(c.admit("e", false), Admit::Flood, "at most four offers per connection");
        assert_eq!(c.admit("a", false), Admit::Fetch, "the same offer after a map change");
        assert_eq!(c.admit("b", true), Admit::Busy);
        c.refused.insert("c".into());
        assert_eq!(c.admit("c", false), Admit::Refused, "a refusal holds for the connection");
    }
}
