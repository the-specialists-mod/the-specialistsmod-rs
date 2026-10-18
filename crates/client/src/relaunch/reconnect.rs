//! `<gamedir>/cl_plugins_data/.reconnect` (docs/implementation/client-plugins.md §13.5 case 4):
//! written when the client quits to restart without a launcher, read once at the next start,
//! which offers "Reconnect to <server>?" within [`FRESH_SECS`]. It never holds a password, only
//! whether one is needed:
//!
//! ```toml
//! format = 1
//! address = "203.0.113.5:27015"
//! server_name = "Bob's TS"
//! password_required = true
//! time = 1792300000        # Unix seconds when it was written
//! ```

use super::args::valid_address;

/// How long the file is offered after it was written.
pub const FRESH_SECS: u64 = 600;
/// Its path under the game directory.
pub const PATH: &str = "cl_plugins_data/.reconnect";

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Reconnect {
    pub format: u32,
    pub address: String,
    #[serde(default)]
    pub server_name: String,
    #[serde(default)]
    pub password_required: bool,
    pub time: u64,
}

fn toml_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if c.is_control() => {}
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

impl Reconnect {
    pub fn new(address: &str, server_name: &str, password_required: bool, time: u64) -> Reconnect {
        Reconnect { format: 1, address: address.to_owned(), server_name: server_name.to_owned(), password_required, time }
    }

    pub fn to_text(&self) -> String {
        format!(
            "# Written by The Specialists port: offered at the next start (docs/implementation/client-plugins.md §13.5).\nformat = 1\naddress = {}\nserver_name = {}\npassword_required = {}\ntime = {}\n",
            toml_str(&self.address),
            toml_str(&self.server_name),
            self.password_required,
            self.time
        )
    }

    /// The file's contents, if they are a valid format-1 file with a valid address.
    pub fn parse(text: &str) -> Option<Reconnect> {
        let r: Reconnect = toml::from_str(text).ok()?;
        (r.format == 1 && valid_address(&r.address)).then_some(r)
    }

    /// Written at most [`FRESH_SECS`] before `now` (and not in the future, give or take a minute).
    pub fn fresh(&self, now: u64) -> bool {
        self.time <= now + 60 && now.saturating_sub(self.time) <= FRESH_SECS
    }

    pub fn label(&self) -> String {
        if self.server_name.is_empty() { self.address.clone() } else { format!("{} ({})", self.server_name, self.address) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_age() {
        let r = Reconnect::new("203.0.113.5:27015", "Bob's \"TS\"\n", true, 1_000_000);
        let text = r.to_text();
        let back = Reconnect::parse(&text).unwrap();
        assert_eq!(back.address, "203.0.113.5:27015");
        assert_eq!(back.server_name, "Bob's \"TS\"");
        assert!(back.password_required);
        assert!(back.fresh(1_000_000 + FRESH_SECS));
        assert!(!back.fresh(1_000_000 + FRESH_SECS + 1));
        assert!(!back.fresh(1_000_000 - 61));
        assert!(Reconnect::parse("format = 1\naddress = \"x;quit\"\ntime = 1\n").is_none());
        assert!(Reconnect::parse("garbage").is_none());
    }
}
