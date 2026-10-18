//! The stub client extension's helper process (`cargo xtask client-plugins`): started through
//! `verified_spawn`, it writes `spawned.txt` (its arguments) in its working directory, the
//! extension's data directory, and exits.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let _ = std::fs::write("spawned.txt", format!("helper ran: {}\n", args.join(" ")));
}
