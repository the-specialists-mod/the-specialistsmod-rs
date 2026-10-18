//! `ts/plugin_offers.toml` (`docs/implementation/client-plugins.md` §6.1, §13.2): the offers, the
//! `[[block]]` list and `[policy]`. Parsed strictly (unknown keys refuse the file); an entry that
//! breaks a rule is dropped with the reason, and the rest stay.
//!
//! `match`, `[[block]]` and `[policy] unlisted` are parsed and kept here; `super::enforce`
//! applies them (§13.3, issue #302).

use plugin_pkg::{Sha256, Version, VersionReq};
use serde::Deserialize;
use shared::protocol::tspkg;

/// The file's name, in the game directory.
pub const FILE: &str = "plugin_offers.toml";

/// An offer's URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Url {
    /// A path under the server's `sv_downloadurl`, which is also the package's path under the
    /// game directory (FastDL serves the game directory).
    Relative(String),
    /// An absolute `https://` URL.
    Https(String),
}

/// A `match` rule (§13.2): which loaded versions of an offered plugin satisfy the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchRule {
    /// The offered version only.
    Exact,
    /// Cargo's caret rule against the offered version: the same major from 1.0.0, the same
    /// minor before it. The default.
    Compatible,
    Any,
    Req(VersionReq),
}

impl MatchRule {
    pub fn parse(s: &str) -> Result<MatchRule, String> {
        match s.trim() {
            "exact" => Ok(MatchRule::Exact),
            "compatible" => Ok(MatchRule::Compatible),
            "any" => Ok(MatchRule::Any),
            r => VersionReq::parse(r)
                .map(MatchRule::Req)
                .map_err(|e| format!("match {r:?} isn't exact, compatible, any or a requirement: {e}")),
        }
    }

    /// The rule as a semver requirement, for op 3 (`=0.2.0`, `^0.2.0`, `*`, or the requirement).
    pub fn requirement(&self, offered: &Version) -> String {
        match self {
            MatchRule::Exact => format!("={offered}"),
            MatchRule::Compatible => format!("^{offered}"),
            MatchRule::Any => "*".into(),
            MatchRule::Req(r) => r.to_string(),
        }
    }

    /// Whether `loaded` satisfies the rule for an offer of `offered`.
    pub fn accepts(&self, offered: &Version, loaded: &Version) -> bool {
        match self {
            MatchRule::Exact => loaded == offered,
            MatchRule::Compatible => VersionReq::parse(&format!("^{offered}")).is_ok_and(|r| r.matches(loaded)),
            MatchRule::Any => true,
            MatchRule::Req(r) => r.matches(loaded),
        }
    }
}

/// An `[[offer]]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub id: String,
    pub version: Version,
    pub url: Url,
    /// SHA-256 of the `.tspkg`.
    pub sha256: Sha256,
    pub size: u32,
    pub required: bool,
    pub note: String,
    pub match_rule: MatchRule,
    /// The package on the server's disk, relative to the game directory: `file`, or a relative
    /// URL's path.
    pub local: String,
}

/// What `[[block]]` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockId {
    Plugin(String),
    /// `manual:<name>`, a hand-installed library.
    Manual(String),
    /// `manual:*`, every hand-installed library.
    AnyManual,
}

impl BlockId {
    pub fn parse(s: &str) -> Option<BlockId> {
        match s.strip_prefix("manual:") {
            Some("*") => Some(BlockId::AnyManual),
            Some(name) if plugin_pkg::manifest::is_valid_id(name) => Some(BlockId::Manual(name.into())),
            None if plugin_pkg::manifest::is_valid_id(s) => Some(BlockId::Plugin(s.into())),
            _ => None,
        }
    }

    /// Whether a plugin the client reports as `id` is this one.
    pub fn matches(&self, id: &str) -> bool {
        match self {
            BlockId::Plugin(p) => p == id,
            BlockId::Manual(n) => id.strip_prefix("manual:") == Some(n),
            BlockId::AnyManual => id.starts_with("manual:"),
        }
    }
}

/// An action for a blocked or unlisted plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Suspend,
    Kick,
}

/// A `[[block]]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub id: BlockId,
    /// `versions`; `*` blocks every version.
    pub versions: VersionReq,
    pub action: Action,
    pub reason: String,
}

/// `[policy] unlisted`: plugins in neither table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Unlisted {
    #[default]
    Allow,
    Suspend,
    Kick,
}

/// The parsed file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offers {
    pub offers: Vec<Offer>,
    pub blocks: Vec<Block>,
    pub unlisted: Unlisted,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    offer: Vec<RawOffer>,
    #[serde(default)]
    block: Vec<RawBlock>,
    #[serde(default)]
    policy: RawPolicy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOffer {
    id: String,
    version: String,
    url: String,
    sha256: String,
    size: u64,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    note: String,
    #[serde(default, rename = "match")]
    match_rule: Option<String>,
    #[serde(default)]
    file: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBlock {
    id: String,
    versions: String,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    #[serde(default)]
    unlisted: Option<String>,
}

/// Parses the file. A file that isn't valid TOML for this format gives no offers and one
/// error; otherwise each bad entry is dropped with its error.
pub fn parse(text: &str) -> (Offers, Vec<String>) {
    let raw: RawFile = match toml::from_str(text) {
        Ok(r) => r,
        Err(e) => return (Offers::default(), vec![format!("{FILE}: {}", e.to_string().trim())]),
    };
    let mut errors = Vec::new();
    let mut out = Offers::default();
    for (i, o) in raw.offer.into_iter().enumerate() {
        let what = format!("{FILE}: [[offer]] {} ({:?})", i + 1, o.id);
        match offer(o) {
            Ok(o) if out.offers.iter().any(|x| x.id == o.id) => errors.push(format!("{what}: a second offer for the id")),
            Ok(o) => out.offers.push(o),
            Err(e) => errors.push(format!("{what}: {e}")),
        }
    }
    for (i, b) in raw.block.into_iter().enumerate() {
        let what = format!("{FILE}: [[block]] {} ({:?})", i + 1, b.id);
        match block(b) {
            Ok(b) => out.blocks.push(b),
            Err(e) => errors.push(format!("{what}: {e}")),
        }
    }
    match raw.policy.unlisted.as_deref().map(str::trim) {
        None | Some("allow") => {}
        Some("suspend") => out.unlisted = Unlisted::Suspend,
        Some("kick") => out.unlisted = Unlisted::Kick,
        Some(v) => errors.push(format!("{FILE}: [policy] unlisted {v:?} isn't allow, suspend or kick; allowing")),
    }
    if out.offers.len() > MAX_CLIENT_OFFERS {
        errors.push(format!(
            "{FILE}: {} offers, but a client shows at most {MAX_CLIENT_OFFERS} per connection (§7.3); the rest are ignored there",
            out.offers.len()
        ));
    }
    (out, errors)
}

/// How many offers a client queues per server per connection (§6.3, §7.3).
pub const MAX_CLIENT_OFFERS: usize = 4;

fn offer(o: RawOffer) -> Result<Offer, String> {
    if !plugin_pkg::manifest::is_valid_id(&o.id) {
        return Err("id isn't [a-z0-9_-]{1,32}".into());
    }
    if o.version.len() > tspkg::MAX_VERSION {
        return Err(format!("version is longer than {} bytes", tspkg::MAX_VERSION));
    }
    let version = Version::parse(o.version.trim()).map_err(|e| format!("version {:?}: {e}", o.version))?;
    let sha256: Sha256 = o.sha256.trim().parse().map_err(|e| format!("sha256: {e}"))?;
    let size = u32::try_from(o.size).map_err(|_| format!("size {} is over {} (TSPkg's size is 32 bits)", o.size, u32::MAX))?;
    tspkg::check_text("note", &o.note, tspkg::MAX_NOTE).map_err(|e| e.to_string())?;
    let match_rule = o.match_rule.as_deref().map(MatchRule::parse).transpose()?.unwrap_or(MatchRule::Compatible);
    let url = o.url.trim();
    if !url.ends_with(plugin_pkg::package::PACKAGE_EXTENSION) {
        return Err(format!("url {url:?} doesn't end with .tspkg (the client finds the manifest beside it)"));
    }
    let url = if let Some(rest) = url.strip_prefix("https://") {
        if rest.is_empty() || url.len() > tspkg::MAX_URL || !url.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b';') {
            return Err(format!("url {url:?} isn't a printable https:// URL of at most {} bytes", tspkg::MAX_URL));
        }
        Url::Https(url.into())
    } else if url.contains("://") {
        return Err(format!("url {url:?}: an absolute URL must be https:// (relative ones use sv_downloadurl)"));
    } else {
        plugin_pkg::check_path(url).map_err(|e| format!("url {url:?}: {e}"))?;
        Url::Relative(url.into())
    };
    let local = match (&o.file, &url) {
        (Some(f), _) => {
            plugin_pkg::check_path(f).map_err(|e| format!("file {f:?}: {e}"))?;
            f.clone()
        }
        (None, Url::Relative(u)) => u.clone(),
        (None, Url::Https(_)) => {
            return Err("an https:// url needs `file`, the package on this server's disk, so the server can check it".into());
        }
    };
    Ok(Offer { id: o.id, version, url, sha256, size, required: o.required, note: o.note, match_rule, local })
}

fn block(b: RawBlock) -> Result<Block, String> {
    let id = BlockId::parse(b.id.trim()).ok_or("id isn't a plugin id, manual:<name> or manual:*")?;
    let versions = b.versions.trim();
    if versions.len() > tspkg::MAX_REQUIREMENT {
        return Err(format!("versions is longer than {} bytes", tspkg::MAX_REQUIREMENT));
    }
    let versions = VersionReq::parse(versions).map_err(|e| format!("versions {versions:?}: {e}"))?;
    let action = match b.action.as_deref().map(str::trim) {
        None | Some("suspend") => Action::Suspend,
        Some("kick") => Action::Kick,
        Some(a) => return Err(format!("action {a:?} isn't suspend or kick")),
    };
    tspkg::check_text("reason", &b.reason, tspkg::MAX_NOTE).map_err(|e| e.to_string())?;
    Ok(Block { id, versions, action, reason: b.reason })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "c3d1000000000000000000000000000000000000000000000000000000000000";

    fn spec_example() -> String {
        format!(
            r#"
[[offer]]
id = "webview"
version = "0.1.0"
url = "plugins/webview-0.1.0.tspkg"
sha256 = "{SHA}"
size = 131072000
required = false
note = "Needed for the stats and vote pages."

[[offer]]
id = "stats"
version = "0.2.0"
url = "https://cdn.example.org/stats-0.2.0.tspkg"
file = "plugins/stats-0.2.0.tspkg"
sha256 = "{SHA}"
size = 10
required = true
match = ">=0.2.0, <0.4"

[[block]]
id = "webview"
versions = "<0.2.0"
action = "kick"
reason = "0.1.x breaks the vote page"

[[block]]
id = "manual:*"
versions = "*"

[policy]
unlisted = "suspend"
"#
        )
    }

    #[test]
    fn parses_the_spec_example() {
        let (o, errors) = parse(&spec_example());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(o.offers.len(), 2);
        let w = &o.offers[0];
        assert_eq!((w.id.as_str(), w.version.to_string(), w.size, w.required), ("webview", "0.1.0".into(), 131_072_000, false));
        assert_eq!(w.url, Url::Relative("plugins/webview-0.1.0.tspkg".into()));
        assert_eq!(w.local, "plugins/webview-0.1.0.tspkg");
        assert_eq!(w.match_rule, MatchRule::Compatible, "the default");
        assert_eq!(w.sha256.to_hex(), SHA);
        let s = &o.offers[1];
        assert_eq!(s.url, Url::Https("https://cdn.example.org/stats-0.2.0.tspkg".into()));
        assert_eq!(s.local, "plugins/stats-0.2.0.tspkg");
        assert!(matches!(s.match_rule, MatchRule::Req(_)));
        assert_eq!(o.blocks.len(), 2);
        assert_eq!(o.blocks[0].action, Action::Kick);
        assert_eq!(
            o.blocks[1],
            Block { id: BlockId::AnyManual, versions: VersionReq::STAR, action: Action::Suspend, reason: String::new() }
        );
        assert_eq!(o.unlisted, Unlisted::Suspend);
        assert_eq!(parse(""), (Offers::default(), vec![]));
    }

    #[test]
    fn bad_entries_are_dropped() {
        let one = |body: &str| {
            let (o, errors) = parse(&format!("[[offer]]\nid = \"a\"\nversion = \"1.0.0\"\nsha256 = \"{SHA}\"\nsize = 1\n{body}\n"));
            assert!(o.offers.len() + errors.len() == 1, "{errors:?}");
            errors.into_iter().next()
        };
        assert_eq!(one("url = \"p/a.tspkg\""), None);
        assert!(one("url = \"p/a.zip\"").unwrap().contains(".tspkg"));
        assert!(one("url = \"../a.tspkg\"").is_some());
        assert!(one("url = \"/abs/a.tspkg\"").is_some());
        assert!(one("url = \"http://x.org/a.tspkg\"").unwrap().contains("https"));
        assert!(one("url = \"https://x.org/a.tspkg\"").unwrap().contains("needs `file`"));
        assert_eq!(one("url = \"https://x.org/a.tspkg\"\nfile = \"p/a.tspkg\""), None);
        assert!(one("url = \"https://x.org/a b.tspkg\"\nfile = \"p/a.tspkg\"").is_some());
        assert!(one("url = \"p/a.tspkg\"\nnote = \"line\\nbreak\"").is_some());
        assert!(one(&format!("url = \"p/a.tspkg\"\nnote = \"{}\"", "n".repeat(65))).is_some());
        assert!(one("url = \"p/a.tspkg\"\nmatch = \"newest\"").is_some());
        let (_, e) = parse(&format!("[[offer]]\nid = \"A\"\nversion = \"1.0.0\"\nurl = \"a.tspkg\"\nsha256 = \"{SHA}\"\nsize = 1\n"));
        assert!(e[0].contains("id"), "{e:?}");
        let (_, e) = parse(&format!("[[offer]]\nid = \"a\"\nversion = \"1.0\"\nurl = \"a.tspkg\"\nsha256 = \"{SHA}\"\nsize = 1\n"));
        assert!(e[0].contains("version"), "{e:?}");
        let (_, e) =
            parse(&format!("[[offer]]\nid = \"a\"\nversion = \"1.0.0\"\nurl = \"a.tspkg\"\nsha256 = \"{SHA}\"\nsize = 5000000000\n"));
        assert!(e[0].contains("32 bits"), "{e:?}");
        let (_, e) = parse("[[offer]]\nid = \"a\"\nversion = \"1.0.0\"\nurl = \"a.tspkg\"\nsha256 = \"xyz\"\nsize = 1\n");
        assert!(e[0].contains("sha256"), "{e:?}");
        // A second offer for an id is dropped; the first stays.
        let entry = format!("[[offer]]\nid = \"a\"\nversion = \"1.0.0\"\nurl = \"a.tspkg\"\nsha256 = \"{SHA}\"\nsize = 1\n");
        let (o, e) = parse(&format!("{entry}{entry}"));
        assert_eq!((o.offers.len(), e.len()), (1, 1));
        // More offers than a client shows: kept, with a warning.
        let five: String = (0..5)
            .map(|i| format!("[[offer]]\nid = \"p{i}\"\nversion = \"1.0.0\"\nurl = \"p{i}.tspkg\"\nsha256 = \"{SHA}\"\nsize = 1\n"))
            .collect();
        let (o, e) = parse(&five);
        assert_eq!((o.offers.len(), e.len()), (5, 1));
    }

    #[test]
    fn the_whole_file_is_refused_for_unknown_keys() {
        let (o, e) = parse("[[offer]]\nid = \"a\"\nhash = \"x\"\n");
        assert_eq!(o, Offers::default());
        assert_eq!(e.len(), 1);
        assert!(parse("[surprise]\n").1[0].contains(FILE));
        assert!(parse("not toml").1.len() == 1);
    }

    #[test]
    fn blocks_and_policy() {
        let b = |s: &str| parse(&format!("[[block]]\n{s}\n"));
        assert!(b("id = \"x\"\nversions = \"<1\"\naction = \"ban\"").1[0].contains("action"));
        assert!(b("id = \"manual:\"\nversions = \"*\"").1[0].contains("id"));
        assert!(b("id = \"x\"\nversions = \"one\"").1[0].contains("versions"));
        assert_eq!(b("id = \"manual:dev\"\nversions = \"*\"").0.blocks[0].id, BlockId::Manual("dev".into()));
        assert!(parse("[policy]\nunlisted = \"ban\"\n").1[0].contains("unlisted"));
        assert!(BlockId::AnyManual.matches("manual:x") && !BlockId::AnyManual.matches("webview"));
        assert!(BlockId::Manual("x".into()).matches("manual:x") && !BlockId::Manual("x".into()).matches("x"));
        assert!(BlockId::Plugin("x".into()).matches("x") && !BlockId::Plugin("x".into()).matches("manual:x"));
    }

    #[test]
    fn match_rules() {
        let v = |s: &str| Version::parse(s).unwrap();
        let m = |s: &str| MatchRule::parse(s).unwrap();
        assert!(m("exact").accepts(&v("0.2.0"), &v("0.2.0")) && !m("exact").accepts(&v("0.2.0"), &v("0.2.1")));
        // Cargo's caret rule: the minor before 1.0, the major after.
        assert!(m("compatible").accepts(&v("0.2.0"), &v("0.2.9")));
        assert!(!m("compatible").accepts(&v("0.2.0"), &v("0.3.0")));
        assert!(!m("compatible").accepts(&v("0.2.0"), &v("0.1.9")));
        assert!(m("compatible").accepts(&v("1.2.0"), &v("1.9.0")));
        assert!(!m("compatible").accepts(&v("1.2.0"), &v("2.0.0")));
        assert!(!m("compatible").accepts(&v("1.2.0"), &v("1.1.0")));
        assert!(m("any").accepts(&v("1.0.0"), &v("0.0.1")));
        assert!(m(">=0.2.0, <0.4").accepts(&v("0.2.0"), &v("0.3.5")) && !m(">=0.2.0, <0.4").accepts(&v("0.2.0"), &v("0.4.0")));
        // Op 3's requirement says the same as `accepts`.
        for rule in ["exact", "compatible", "any", ">=0.2.0, <0.4"] {
            let req = VersionReq::parse(&m(rule).requirement(&v("0.2.0"))).unwrap();
            for loaded in ["0.1.9", "0.2.0", "0.2.5", "0.3.0", "0.4.0", "1.0.0"] {
                assert_eq!(req.matches(&v(loaded)), m(rule).accepts(&v("0.2.0"), &v(loaded)), "{rule} {loaded}");
            }
        }
    }
}
