//! Version changes between an approved plugin and the one found or offered (§5.2, §6.4).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use semver::Version;

use crate::manifest::Manifest;
use crate::platform::Platform;

/// How a found or offered version relates to the approved one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionChange {
    /// The same version.
    Same,
    /// Newer: the update card.
    Upgrade,
    /// Older: the update card shows a downgrade warning, and "Allow" isn't the default (§6.4).
    Downgrade,
    /// The same precedence but different build metadata (`1.0.0+a` and `1.0.0+b`): neither
    /// newer nor older, but still a different version that needs approval.
    Rebuild,
}

impl VersionChange {
    /// Compares `found` with `approved` by semver precedence (pre-releases sort before their
    /// release; build metadata doesn't order).
    pub fn between(approved: &Version, found: &Version) -> VersionChange {
        if approved == found {
            return VersionChange::Same;
        }
        match found.cmp_precedence(approved) {
            Ordering::Greater => VersionChange::Upgrade,
            Ordering::Less => VersionChange::Downgrade,
            Ordering::Equal => VersionChange::Rebuild,
        }
    }

    /// Whether this is a downgrade.
    pub fn is_downgrade(self) -> bool {
        self == VersionChange::Downgrade
    }
}

/// What the update card shows for an offer of `new` where `old` is approved (§6.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateSummary {
    /// Upgrade, downgrade (warned, "Allow" not the default), rebuild or same.
    pub version: VersionChange,
    /// Whether the publisher key is the same (both unsigned counts as the same).
    pub same_key: bool,
    /// The `[declares]` keys that changed: `processes`, `network`, `data`.
    pub declares_changed: Vec<&'static str>,
    /// How many of the platform's files were added, removed or changed.
    pub files_changed: usize,
}

impl UpdateSummary {
    /// Compares two manifests of one plugin for `platform`.
    pub fn between(old: &Manifest, new: &Manifest, platform: Platform) -> UpdateSummary {
        let key = |m: &Manifest| m.publisher.as_ref().and_then(|p| p.key);
        let (o, n) = (&old.declares, &new.declares);
        let mut declares_changed = Vec::new();
        if o.processes != n.processes {
            declares_changed.push("processes");
        }
        if o.network != n.network {
            declares_changed.push("network");
        }
        if o.data != n.data {
            declares_changed.push("data");
        }
        let files = |m: &Manifest| m.files_for(platform).map(|f| (f.path.clone(), f.sha256)).collect::<BTreeMap<_, _>>();
        let (of, nf) = (files(old), files(new));
        let files_changed = of.iter().filter(|(p, h)| nf.get(*p) != Some(h)).count() + nf.keys().filter(|p| !of.contains_key(*p)).count();
        UpdateSummary {
            version: VersionChange::between(&old.version, &new.version),
            same_key: key(old) == key(new),
            declares_changed,
            files_changed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{key, sample};

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn update_summary() {
        let old = Manifest::parse(sample("1.0.0", Some(&key(1)), None).manifest.as_bytes()).unwrap();
        let mut new = Manifest::parse(sample("0.9.0", Some(&key(2)), None).manifest.as_bytes()).unwrap();
        new.declares.network = true;
        new.files.retain(|f| f.path != "data/readme.txt");
        new.files[0].sha256 = crate::Sha256::of_bytes(b"new build");
        let s = UpdateSummary::between(&old, &new, Platform::LinuxX86_64);
        assert_eq!(s.version, VersionChange::Downgrade);
        assert!(!s.same_key);
        assert_eq!(s.declares_changed, ["network"]);
        assert_eq!(s.files_changed, 2, "the rebuilt library and the removed readme");
        let same = UpdateSummary::between(&old, &old, Platform::LinuxX86_64);
        assert_eq!((same.version, same.same_key, same.files_changed), (VersionChange::Same, true, 0));
    }

    #[test]
    fn downgrade_detection() {
        use VersionChange::*;
        assert_eq!(VersionChange::between(&v("0.1.0"), &v("0.1.0")), Same);
        assert_eq!(VersionChange::between(&v("0.1.0"), &v("0.2.0")), Upgrade);
        assert_eq!(VersionChange::between(&v("0.2.0"), &v("0.1.0")), Downgrade);
        assert_eq!(VersionChange::between(&v("0.10.0"), &v("0.9.0")), Downgrade, "numeric, not lexical");
        assert_eq!(VersionChange::between(&v("1.0.0"), &v("1.0.0-rc.1")), Downgrade, "a pre-release is older than its release");
        assert_eq!(VersionChange::between(&v("1.0.0-rc.1"), &v("1.0.0")), Upgrade);
        assert_eq!(VersionChange::between(&v("1.0.0-rc.2"), &v("1.0.0-rc.10")), Upgrade);
        assert_eq!(VersionChange::between(&v("1.0.0+a"), &v("1.0.0+b")), Rebuild);
        assert!(VersionChange::between(&v("2.0.0"), &v("1.9.9")).is_downgrade());
        assert!(!VersionChange::between(&v("1.0.0+a"), &v("1.0.0+b")).is_downgrade());
    }
}
