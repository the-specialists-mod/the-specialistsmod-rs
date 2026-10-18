# Client plugins: packages, downloads and approval

Status: specification, 2026-10-17; partly built. Built so far: the pure parts (§3, §5, §10:
packages, manifests, signatures, the store and verification) in `crates/plugin-pkg` (issue #296);
the client extension API and its loader ([client-ext-api.md](client-ext-api.md), issue #293);
milestone 2 (issue #297): verification at every start, the start-up prompt, hand-installed
libraries, `[requires] port`, `scope = "server"` loading, the `cl_plugins*` cvars,
`cl_plugins_list`, `cl_plugins_verify` and `cargo xtask plugin-approve`, in
`crates/client/src/plugins/` (§16 lists what that build decided); the server's offers (§6.1, §6.2,
§6.5, §12, op 5 of §13.1; `ts_server::plugin_offers`, `shared::protocol::tspkg`, issue #298); the
port client's version check (§13.1, issue #300); the port side of restart and reconnect (§13.5,
issue #301; the launcher side is tsmod-launcher #4 and #5; `cargo xtask relaunch` passes on FWGS
Linux and FWGS win32 under Wine since 2026-10-19, and the self-relaunch is on for those two
platforms, validation.md §2.r); and the client's offer flow (§6.3,
§6.4, §7.2, §7.3, §9, op 5's card; `crates/client/src/plugins/offers/`, issue #299; §17 lists what
that build decided); and enforcement (§13.2–§13.4: `match`, `[[block]]`, `[policy] unlisted`, ops 3
and 4, suspend and resume, the grace periods; `ts_server::plugin_offers::enforce` and
`ts_client::plugins::offers::enforce`, issue #302; §18 lists what that build decided); and the
`cl_plugins_menu` panel (§8; `crates/client/src/plugins/menu/`, issue #311; §19 lists what that
build decided).

The server loads `tsmod_*` libraries from `dlls/` ([plugin-api.md](plugin-api.md) §1.2); this
document gives the client the same, with the player's approval. It adds two things:

* **Client plugins:** native libraries that `client.dll` loads through the client extension API.
  That API is [client-ext-api.md](client-ext-api.md) (first specified in
  `the-specialists-mod/tsmod-plugin-webview-rs` `docs/specifications.md` §4.1). Its loader asks
  an approval gate about every library before mapping it; the store described here plugs into
  that gate (client-ext-api.md §3).
* **Packages and approval:** how plugins are installed, downloaded from a server, and approved by
  the player. Every file of an approved plugin is hashed, and nothing loads unless every hash
  still matches.

The first plugin is the webview overlay (`tsmod-plugin-webview-rs`).

## 1. Goals and non-goals

Goals:

* **Nothing loads without the player's approval.** That applies to a plugin the player copied in
  by hand, one the launcher installed, and one a server offered.
* **An approval covers exact bytes.** It records a SHA-256 of every file in the plugin. If any
  file changes, disappears or appears, for any reason, the plugin doesn't load and the player is
  asked again, and told what changed.
* **Servers can require matching versions** of the port client and of each plugin, and block
  plugin versions they don't want; mismatched clients are refused or told exactly what to get
  (§13).
* **Servers can offer plugins.** The player sees the details before anything is downloaded
  (name, publisher, version, size, signature, what it says it does) and approves or refuses.
  A server can mark a plugin as required; it can't install one.
* **Works on every client the port supports:** Steam HL25 and FWGS, Linux and Windows, 32- and
  64-bit.

Non-goals:

* **Sandboxing.** A plugin is native code running in the game process with the player's
  permissions. Approval is consent, not containment, and the prompts say so plainly. The
  manifest's `declares` (§3.2) is informational only.
* **Protection against malware already on the computer.** Something that can write to the game
  directory can also edit the approval store. The store protects against servers and against
  silent changes, not against a local attacker.
* **Unloading.** Once loaded, a plugin stays loaded until the game exits (§9).
* macOS.

## 2. Overview

```
        server (port)                              client (port client.dll)
┌──────────────────────────┐   TSPkg offer    ┌──────────────────────────────────────────┐
│ plugin_offers.toml       │ ───────────────► │ 1. fetch manifest + signature (KB)       │
│ sv_plugin_offer, API     │                  │ 2. offer card: details, size, signature  │
│                          │ ◄─────────────── │ 3. player approves → download package    │
│ FastDL / HTTPS host      │   tspkg <state>  │ 4. verify: signature, package hash,      │
│  plugins/<id>-<v>.tspkg  │ ◄── HTTP(S) ──── │    every file's hash                     │
└──────────────────────────┘                  │ 5. install to cl_plugins/<id>/<version>/ │
                                              │ 6. record approval with every file hash  │
                                              │ 7. load through the client extension API │
                                              └──────────────────────────────────────────┘
 At every start: hash every installed plugin's files and compare them with approved.toml.
 A match loads; anything else prompts and doesn't load until it's approved.
```

## 3. Packages

### 3.1 Format

A package is a zip file named `<id>-<version>.tspkg`, holding:

```
manifest.toml
manifest.sig          ed25519 signature of manifest.toml's exact bytes: the raw 64 bytes
                      (absent when unsigned)
publisher.cert        the countersignature (§3.3; optional, only with manifest.sig)
files/...             the plugin's files, paths as in the manifest
```

* **Zip rules:** nothing else may be in the zip. Directory entries are allowed only for the
  directories of listed files (`zip -r` writes them). Entries must be stored or deflated, not
  encrypted, and not symlinks or other special files. No two names may be equal ignoring case,
  and every local header must match the central directory (a zip can otherwise hide a second
  copy of a name). The zip's own file modes are ignored: unpacking makes only `declares.processes`
  executable (on Linux). Sizes are enforced while reading, so an entry can't inflate past its
  manifest size.
* The manifest and its signature are also served next to the package, as
  `<id>-<version>.manifest.toml` and `.manifest.sig`, and its countersignature as
  `.publisher.cert` when it has one (the card shows the trust level, §3.3), so the client can show
  details before it downloads anything (§6.3). `cargo xtask plugin-offer` writes them, and the
  server refuses to offer a package whose siblings are missing or differ from its own files.
* The `.tspkg` extension isn't on either engine's list of blocked download extensions. The port
  downloads packages itself, though, not through the engine (§6.3).

### 3.2 Manifest

```toml
format = 1
id = "webview"                     # [a-z0-9_-]{1,32}; the library is tsmod_<id><suffix>
name = "Web overlay"
version = "0.1.0"                  # semver, at most 16 characters (TSPkg's limit, §6.2)
description = "A Chromium overlay that servers and the player can open."
homepage = "https://github.com/the-specialists-mod/tsmod-plugin-webview-rs"
license = "MIT"
client_ext_api = 1                 # the extension API version it was built for (exact)

[publisher]                         # optional for unsigned packages
name = "The Specialists Mod"
key = "ed25519:BASE64..."          # the key manifest.sig verifies with (standard base64, 32 bytes)

[requires]                          # optional
port = ">=3.0.8, <3.1"             # semver requirement on the port client's version (§13.5)

[platforms]                         # the library to load per client platform
linux-i686    = "lib/linux-i686/tsmod_webview.so"
linux-x86_64  = "lib/linux-x86_64/tsmod_webview.so"
windows-i686  = "lib/windows-i686/tsmod_webview.dll"
windows-x86_64 = "lib/windows-x86_64/tsmod_webview.dll"

[declares]                          # informational; shown on the card, not enforced
processes = ["bin/tsmod-webview-host"]   # executables the plugin starts
network = true
data = true                         # writes to its data directory (§4)

[[files]]                           # every file in the package, no exceptions
path = "lib/linux-x86_64/tsmod_webview.so"
size = 2412544
sha256 = "9f2c..."
platform = "linux-x86_64"           # optional: only installed on that platform
# ...
```

* **Every file is listed.** A file in `files/` without an entry, or an entry without a file,
  rejects the package.
* **Paths** are relative, use `/`, and may not contain `..`, a drive letter, or a leading `/`.
  Symlinks and other special zip entries reject the package. So that a path names the same
  file on Linux and Windows, the full rules (`plugin_pkg::path`) are also: printable ASCII
  without `\ : * ? " < > |`, at most 160 bytes, no empty or `.` segments, no segment ending in
  `.` or a space, no Windows device names (`CON`, `nul.txt`, `COM1`...), no two paths equal
  ignoring case, and no path that is also another's directory. The same rules apply to the
  store's paths and to a `.files` list (§5.3), on unpacking and at every verification.
* **Strict parsing:** `format` must be 1, unknown keys are refused (a new key needs a new
  `format`), and the platform libraries and `declares.processes` must all be in `files` (a
  platform's library installed on that platform).
  Names and descriptions may not hold control or bidirectional-override characters, since the
  cards show them; `homepage` must be `http(s)://`. At most 64 KB of manifest and 4 GiB of
  files per platform.
* **`[requires] port`** is read and kept by `plugin-pkg`. The loader enforces it: a plugin whose
  requirement the running port doesn't meet is skipped at start with the reason (§13.5).
* **Platforms:** a package that has no library for the client's platform is reported as
  `unsupported` (§6.5) and is never offered to the player.

### 3.3 Signatures and trust

| Level | Meaning | Shown as |
|---|---|---|
| **Verified** | `manifest.sig` checks against `publisher.key`, and that key is countersigned by a key built into the port (the-specialists-mod's release key; the list is `plugin_pkg::BUILTIN_ROOTS`, `crates/plugin-pkg/src/trust.rs`, so the client and the launcher share it) | "Verified publisher: The Specialists Mod" |
| **Known publisher** | the signature checks, and the player has approved a plugin with this key before | "Signed by <name>, approved before on <date>" |
| **New publisher** | the signature checks, the key is new | "Signed by an unknown publisher" plus the key's fingerprint |
| **Unsigned** | no `manifest.sig`, or a library dropped in with no manifest (§5.3) | red: "Unsigned: nobody vouches for this file" |

* Countersignatures ship as `publisher.cert` next to `manifest.sig`, signed by a built-in key.
  It's TOML: `format = 1`, `key` and `name` (the publisher key and name, which must equal the
  manifest's), `signer` (the built-in key) and `signature` (base64 of an ed25519 signature over
  `"tsmod publisher.cert v1\n"`, the key's 32 bytes and the name's UTF-8). The name is bound, so
  "Verified publisher: <name>" always shows the countersigned name.
* **Checking:** signatures are checked strictly (`verify_strict`: no malleable signatures, no
  weak keys). A `manifest.sig` that fails, a signature without `publisher.key`, a malformed
  `publisher.cert`, one for another key or name, one without `manifest.sig`, and one whose
  built-in signer's signature fails all reject the package: a broken signature is never shown
  as "unsigned". A `publisher.cert` from a key that isn't built in is ignored (signed, not
  verified), so retiring a root key doesn't break old packages.
* The **fingerprint** is the first 16 bytes of the key's SHA-256, as 8 groups of 4 hex digits.
* Until the countersigning key exists (open question 4) the built-in list is empty, and no
  package is "verified".
* **Key changes:** a plugin id that was approved under one key and now comes with another is
  treated as a different plugin, with a warning that the publisher changed. Signed and unsigned
  count as different keys. A "Never" is per key too: blocking an id blocks it from that key only.
* **Unsigned code:** unsigned packages from servers are refused unless the player has set
  `cl_plugins_allow_unsigned 1`. Hand-installed unsigned libraries can still be approved through
  the start-up prompt, with the red warning (that's how developers work, §11).

## 4. Install layout (client)

```
ts/cl_plugins/approved.toml                 the approval store (§5)
ts/cl_plugins/<id>/<version>/               an installed package, laid out as in the zip: manifest.toml,
                                            manifest.sig, publisher.cert, files/... (this platform's)
ts/cl_plugins/staging/                      downloads in progress; never loaded
ts/cl_plugins_data/<id>/                    the plugin's own writable data (caches, profiles, settings)
ts/cl_dlls/tsmod_<name><suffix>             hand-installed libraries without a package (§5.3)
```

* **Unpacking** writes to a hidden `cl_plugins/<id>/.<version>.partial/`, which is renamed into
  place, then lists and re-hashes the result, which must match the manifest exactly. Any
  failure removes everything. Directories whose names start with `.`, and `staging/`, are never
  candidates.

* A plugin must write only to its data directory, never to its package directory. Writing there
  changes a hash and triggers a prompt at the next start, so this is self-enforcing.
* One version of a plugin is active at a time. Older version directories are kept until the new
  one is approved, then deleted.

## 5. Approval store and verification

### 5.1 `approved.toml`

```toml
format = 1

[[plugin]]
id = "webview"
version = "0.1.0"
state = "allowed"                 # allowed | blocked
scope = "always"                  # always | server
servers = ["203.0.113.5:27015"]   # for scope = "server"
source = "server 203.0.113.5:27015"   # or "launcher", "manual"
publisher_key = "ed25519:BASE64..."   # absent for unsigned
trust = "verified"                # verified | known | new | unsigned, as shown at approval (§3.3)
approved_at = "2026-10-17T10:42:00Z"
manifest_sha256 = "41ab..."       # absent for hand-installed libraries
platform = "linux-x86_64"

[plugin.files]                    # every file of the installed package, for this platform,
"manifest.toml" = "41ab..."       # relative to cl_plugins/<id>/<version>/
"manifest.sig" = "0c5e..."
"files/lib/linux-x86_64/tsmod_webview.so" = "9f2c..."
"files/bin/tsmod-webview-host" = "77d0..."
# ... every other file
```

* The store is written only by the approval dialogs (§7) and the launcher. Nothing a server
  sends can write it.
* A hand-installed library's entry has `id = "manual:<name>"`, `version = "0.0.0"`, and paths
  relative to `cl_dlls/`.
* Approving replaces every other entry for the same id and platform (one active version, §4).
  "Never" everywhere does too; "Never from this server" adds a `state = "blocked"`,
  `scope = "server"` entry beside them.
* `state = "blocked"` remembers a "Never" choice, so the plugin isn't offered or prompted for
  again. `cl_plugins_menu` (§8) can undo it.
* The store is written atomically (write to a temporary file, then rename). A store that fails
  to parse is set aside as `approved.toml.bad`, and every plugin is prompted for again. That
  includes a store with another `format`, an unknown key, a bad id, or a file path that breaks
  the path rules (§3.2).

### 5.2 Verification at every start

At `Initialize`, before any plugin is mapped:

1. List the candidates: every `cl_plugins/<id>/<version>/` with a manifest, and every
   `cl_dlls/tsmod_*<suffix>` (§5.3).
2. For each, hash every file under its package directory, in parallel on worker threads.
3. Compare the results with the store. The plugin loads only if all of these hold:
   * an entry with `state = "allowed"` exists for this id, version and platform;
   * the set of files is exactly the set in the entry (no missing files, no extra files);
   * every hash matches, and `manifest_sha256` matches;
   * the signature still verifies, and the key is still the entry's `publisher_key`.
4. Anything else is **changed**, and the plugin doesn't load; it goes to the start-up prompt
   (§7.1). Changed means:
   * a file's contents differ;
   * a file is missing, or there's an extra file;
   * the manifest changed;
   * a different version or key;
   * a plugin seen for the first time.
5. Some findings mean the plugin **can't be approved as it is**, and the prompt shows them
   without "Allow" (only "Not now" and "Never"); reinstalling the package is the way out:
   * installed files that disagree with the package's own manifest (a changed, missing or extra
     file under the package directory, with no new signed manifest to vouch for it);
   * a symlink or other special file in the package directory, or an unreadable file;
   * a signature that no longer checks (§3.3);
   * a manifest that is missing, doesn't parse, or names another id or version than its
     directory;
   * no library for this platform.

   Approving such files would record hashes nobody signed, so a tampered install could be
   approved with one click. A hand-installed library (§5.3) has no manifest, so its changes
   are always approvable, except a `.files` list naming a file that isn't there.

* **Time budget:** SHA-256 runs at about 1–2 GB/s per core with SHA extensions. The webview
  package (about 300 MB unpacked, mostly CEF) takes about 0.2 s on four threads. The port logs
  the time, and `cl_plugins_verify_log 1` logs each file.
* **No caching on size or mtime.** Every file is hashed in full at every start, because a cache
  keyed on metadata would miss a change that preserved it.
* **Executables started later:** a plugin that starts a helper process does it through the
  extension API's `verified_spawn(path, args)`. That call re-hashes the file against the
  approval immediately before starting it, and refuses if the file has changed or isn't listed.
  Listed means in the plugin's approval: any of its files (for a hand-installed library, the
  files its `.files` list names, and the list itself).
  The webview plugin's `tsmod-webview-host` and CEF's subprocesses (the same executable) start
  this way. Libraries the helper loads itself (`libcef`) were hashed at start; re-checking them
  is open question 5.

### 5.3 Hand-installed libraries

A `cl_dlls/tsmod_<name><suffix>` with no package is treated as a one-file unsigned plugin:
- **id:** `manual:<name>`; `<name>` must be `[a-z0-9_-]{1,32}`, like a package id
- **files:** the library alone, plus any `tsmod_<name>.files` list next to it naming more files
- **`<suffix>`** is the server's `tsmod_*` suffix (`_i386.so`, `_amd64.so`, `.dll`,
  `_amd64.dll`; `plugin_pkg::Platform::library_suffix`). On win32, `tsmod_*_amd64.dll` isn't a
  candidate.
- **The `.files` list:** one path per line, relative to `cl_dlls/`, under the path rules (§3.2);
  blank lines and lines starting with `#` are ignored. The list itself is one of the approved
  files, so editing it prompts again.

It goes through the start-up prompt with the unsigned warning. The approval covers that one hash,
so every rebuild prompts again. Developers can pre-approve a build instead (§11).

## 6. Server offers

### 6.1 Server side

* **`ts/plugin_offers.toml`** in the server's game directory:

  ```toml
  [[offer]]
  id = "webview"
  version = "0.1.0"
  url = "plugins/webview-0.1.0.tspkg"   # relative to sv_downloadurl, or an absolute https:// URL
  sha256 = "c3d1..."                    # of the .tspkg
  size = 131072000
  required = false                      # true: the server may kick players who refuse (below)
  note = "Needed for the stats and vote pages."   # ≤ 64 bytes, shown on the card
  ```

  `cargo xtask plugin-offer [--required] <tspkg>` checks a built package, writes its siblings
  (§3.1) and prints the entry.
  * **URLs:** a relative `url` is the package's path under the game directory, which is what
    FastDL serves under `sv_downloadurl`. client.dll can't read the engine's download URL, so the
    server sends the full URL: `sv_downloadurl` (trailing `/` dropped), `/`, the path, with the
    `fastdl` flag (§6.2). A relative offer isn't sent while `sv_downloadurl` is empty or isn't
    `http(s)://`, and the log says so. An absolute `url` must be `https://`, and needs
    `file = "<path under the game directory>"`, the same package on the server's disk, so the
    server can check it.
  * **Checks:** at each map start, and on `sv_plugin_offers reload`, the server reads the file and
    checks every offered package before offering it: the size and SHA-256 must be the offer's, the
    package must open with its entries exactly its manifest's files and every file's size and hash
    matching, the manifest must name the offer's id and version, a signature must check
    (`plugin_pkg::builtin_roots` for "verified"), and the siblings must be byte-identical to the
    package's own. A package that fails isn't offered; the log names the reason. Results are
    cached on the package's path, size and modification time, so an unchanged package isn't
    hashed again at every map.
  * **Parsing** is strict: an unknown key, or a file that isn't TOML, gives no offers. A bad
    entry (an id that isn't `[a-z0-9_-]{1,32}`, a version that isn't semver or is over 16 bytes, a
    size over 32 bits, a note over 64 bytes or with control characters, a `url` that doesn't end
    with `.tspkg`, a second offer for the same id) is dropped with its reason. More than 4 offers
    are kept with a warning: a client shows at most 4 per connection (§7.3).
* **When offers are sent:** on every `ClientPutInServer` (so again after each map change; the
  client answers `have` or remembers a refusal for the connection), only to clients that hook
  `TSPkg` (§12). They (and op 5) go out at the player's first `PlayerPostThink` after it: FWGS's
  server drops user messages to a client that isn't spawned yet, which it isn't at
  `ClientPutInServer` (#317). Offers also go out on demand, with `sv_plugin_offer <#userid|name|all> <id>`;
  `sv_plugin_withdraw <#userid|name|all> <id>` sends op 2 for one sent earlier, and
  `sv_plugin_offers` lists the offers and their checks. The plugin API's
  `offer_client_plugin(player, id)` (version 8, #321, plugin-api.md §13) does what
  `sv_plugin_offer` does, for a player or with 0 for all; tsmod-amxx-rs exposes it as a native.
  An AMX Mod X plugin also sees the `tspkg` client command before the game does.
* **Required plugins:** with `sv_plugin_required_kick 1` (default 0), a player who answers
  `denied`, `unsupported` or `failed` to a required offer is kicked. The kick reason names the
  plugin and its homepage ("This server requires the client plugin <name> <version>
  (<homepage>).", from the package's manifest). Without it, the plugin API reports the state and
  plugins decide.

| cvar / command (server, port additions) | default | meaning |
|---|---|---|
| `sv_plugin_required_kick` | 0 | 1 kicks a player who refuses a required offer |
| `sv_plugin_check_time` | 15 | seconds before the op 5 kick (§13.1); the grace period before `match`, `[[block]]` and `unlisted` apply, and the time a client has to answer a suspend (§13.3) |
| `sv_plugin_download_time` | 300 | seconds after the offers go out before a required offer that isn't loaded at a version its `match` accepts kicks, with `sv_plugin_required_kick 1` (§13.3) |
| `ts_dev_tspkg` | 0 | dev aid, with `sv_cheats 1`: every port client counts as hooking `TSPkg` (§12) |
| `sv_plugin_offer <#userid\|name\|all> <id>` | | offers `id` now |
| `sv_plugin_withdraw <#userid\|name\|all> <id>` | | op 2 for an offer sent earlier |
| `sv_plugin_offers [reload]` | | lists the offers, their checks, the blocks and the policy |
* **What the server learns:** the client's state for each offer, through `tspkg` (§6.5), logged as
  `[ts] tspkg: <player>: <id> <version> <state> [reason]`, and the plugin API event
  `client_plugin_state(player, id, version, state, reason)` (version 8, #321; the spec's
  `(edict, id, state)` plus the answer's version and reason).

### 6.2 The `TSPkg` user message

The port registers `TSPkg` after TS's 64 messages and its own `TSExt` (`shared::protocol::Msg::TsPkg`
in the one message table, which both the server's registration and the client's hooks walk), so
TS's registration order is unchanged (design.md §2).

An offer can be longer than GoldSrc's 192-byte user message limit (ReHLDS stops the server with a
`Host_Error` past it), so every body goes in chunks, each one `TSPkg` message, as the webview
spec's `TSExt` framing does (open question 1):

```
byte    op            1 offer, 2 withdraw, 3 require, 4 block, 5 port_version (3 to 5: §13.3)
byte    msg_id        the same on every chunk of one body; wraps at 256
byte    chunk         index from 0; high bit (0x80) set on the last chunk
...     body          at most 189 bytes of the body
```

The server sends a body's chunks back to back in the reliable stream, so the client keeps one
body in progress: a chunk for another `msg_id` or op, or out of order, drops it. A body has at most
4 chunks. The op 1 body:

```
string  id            ≤ 32
string  version       ≤ 16
long    size          package bytes (unsigned)
byte×32 sha256        of the .tspkg
byte    flags         1 required, 2 fastdl (the URL is sv_downloadurl plus the offer's path,
                      and may be plain http://)
string  url           ≤ 255; the full URL of the .tspkg
string  note          ≤ 64
```

Op 2 `withdraw` is `string id, string version`. Strings are NUL-terminated, without control or
bidirectional-override characters; ids, versions and URLs are printable ASCII without spaces,
quotes or `;`, since they go back in `tspkg` replies. `shared::protocol::tspkg` has the encoder
(`Message::encode`) and the client's decoder (`Reassembler`, `Message::decode`).

### 6.3 Client flow

1. **Receive the offer.** The client checks it first:
   * it ignores offers when `cl_plugins_download 0`;
   * it ignores offers for an id blocked in the store, for this server or always;
   * it answers `have` when the same id, version and package hash is already approved, and loads
     it if it isn't loaded yet (scope permitting).
2. **Fetch the manifest** and its signature (`<url without .tspkg>.manifest.toml` / `.sig`, at
   most 64 KB, and `.publisher.cert` when the package is countersigned), on a worker thread.
   Then check:
   * the signature (§3.3);
   * that the id and version match the offer;
   * that there's a library for this platform; if not, answer `unsupported`.
3. **Show the offer card** (§7.2) and answer `offered`. A card waits in a queue while another
   card is open; one server can queue at most 4 offers per connection.
4. **On approval:**
   * Download the package into `staging/`, with progress on the card and the game still
     playable. Answer `downloading`.
   * The size must match the offer, and the download stops if it goes past it.
   * The SHA-256 must match the offer.
5. **Verify the package:**
   * its manifest is byte-identical to the one approved on the card;
   * the zip entries are exactly the manifest's files (§3.2), and every file's hash matches.
6. **Install:** unpack to `cl_plugins/<id>/<version>/`, re-hash the unpacked files, write the
   store entry with those hashes, and delete the staging file.
7. **Load** (§9). Answer `loaded`, or `installed` when the scope means it shouldn't load yet.
   Any failure answers `failed <reason>` and leaves nothing behind but the log line.

* **HTTP:** a worker thread in `client.dll` does the fetching, with `ureq` and `rustls` and
  redirects limited to 3. HTTPS is required unless the offer has the `fastdl` flag: then the URL
  is the server's `sv_downloadurl` plus a path, which is often plain HTTP. That's acceptable because integrity
  comes from the offer's hash and the signature, not the transport. Unsigned packages over plain
  HTTP are always refused. The cost is a TLS stack in `client.dll` (open question 2).
* **Cancel:** disconnecting or pressing Cancel stops the download and deletes the staging file.
* **Built (#299):** `ts_client::plugins::offers` (`flow.rs` decides, `fetch.rs` runs the worker
  threads, `card.rs` lays the cards out, `mod.rs` routes messages, frames, drawing and input).
  * Every offer is fetched first (manifest, `.manifest.sig`, then `.publisher.cert`; a missing
    signature or certificate is a 404, not an error), on a thread of its own; the game polls
    the results once a frame (`HUD_Frame`), so nothing waits on the network or the disk in a
    frame. The download, the package checks, the unpacking and the verification of the install
    run on a worker too.
  * Before the card, in this order: the manifest parses and names the offer's id and version;
    the signature checks (§3.3; a broken one is `failed`); unsigned is `failed` over plain HTTP,
    and unless `cl_plugins_allow_unsigned 1`; a block for this key (here or everywhere) is
    `blocked`; no library for this platform or an unmet `[requires] port` is `unsupported`; another
    `client_ext_api` is `failed api`. `cl_plugins 0` and `cl_plugins_download 0` ignore offers.
  * `have` is the same id, version, platform and `manifest_sha256` approved and applying on this
    server (the store keeps no package hash; the manifest pins every file). The same files
    approved only for other servers get the card, and allowing them needs no download.
  * `offered` is sent when the card is queued; a card's click sends `downloading`; then `loaded`,
    `installed` (approved for other servers only), `restart` (an older version is loaded, §9) or
    `failed <reason>`. "Not now" and "Never" send `denied`, and the id isn't shown again this
    connection. A connection is the server's address: a map change keeps it, a new address or a
    disconnect cancels its downloads and closes its cards.
  * The download goes to `cl_plugins/staging/<id>-<version>.tspkg.part`, refused as soon as it's
    bigger than the offer (or a `Content-Length` disagrees), hashed, opened with `plugin-pkg`
    (its manifest must be the card's byte for byte, and its signature the same key), unpacked
    beside `cl_plugins/<id>/<version>/` and renamed into place (an existing directory of that
    version is set aside as `.<version>.replaced` first), then verified again as installed. The
    approval written is that verification's: into the store, saved at once, and the gate's
    in-memory copy, so `ext::load_one` maps it at once. "Allow on this server only" for files
    already approved on other servers keeps those servers. Any failure leaves only the log line.
  * Transport: `ureq` 3 with `rustls`, ring and the webpki roots (about 1.5 MB per
    `client.dll`, open question 2); HTTPS only, redirects included, unless the offer is FastDL's;
    at most 3 redirects; a non-2xx status is an error.
  * While a download runs, a line at the top of the screen shows it (no cursor; the game goes
    on). Cancel is `cl_plugins_cancel` (port addition; it only stops downloads), disconnecting,
    or the server's withdrawal (op 2, which also closes a card not answered yet).

### 6.4 Updates

An offer for a newer version of an approved plugin shows the update card (§7.2):
- the old and new versions;
- whether the publisher key is the same;
- the `declares` that changed;
- how many files changed.

It's approved like any offer, so every change prompts. An offer for an older version than the
approved one is shown as a downgrade, with a warning, and "Allow" isn't the default.

Built (#299): the update card is the offer card titled "Update: <name> <old> -> <new>", with a
warning line for a downgrade, for the same version rebuilt (another manifest), and for another
publisher key, the files and `declares` that changed (from the installed copy's manifest), and
"A plugin that's loaded now updates at the next start". Its first button is "Install update". No
card's default is ever an approval.

### 6.5 Client replies: `tspkg`

```
tspkg <id> <version> <state> [reason]
state: have | offered | downloading | installed | loaded | denied | blocked | unsupported | failed
       | suspended | restart        (§13.3)
tspkg list <id>@<version> ...       every loaded plugin, sent once after the first TSPkg (§13.2)
tspkg list                          ends the list (a client with no plugins sends only this)
```

* `denied` covers both "Not now" and "Never". The server isn't told which.
* `reason` is the rest of the line, at most 64 bytes; the client quotes it, without quotes or `;`.
  `tspkg list` lines stay under 128 bytes and add to the list until the bare `tspkg list`.
  `shared::protocol::tspkg` has `Reply::command` and `list_commands` (client) and `Reply::parse`
  (server).
* The server rate-limits `tspkg` (16 per 10 seconds per client, a sliding window) and ignores ids
  it didn't offer this connection, `tsrs` unless it sent op 5, and `tspkg list` before any `TSPkg`.

## 7. Approval dialogs

### 7.1 At start-up

Changed or new plugins from §5.2 are shown before the main menu, with `SDL_ShowMessageBox`.
`client.dll` looks it up in the SDL2 the engine has already loaded (`goldsrc::platform`'s
lookup, as the port already does for mouse and joystick calls). Both HL25 and FWGS use SDL2.

* **Contents:**
  * plugin name, version and id;
  * publisher and trust level (§3.3);
  * source;
  * what changed: "new plugin", or the list of changed, missing and extra files (up to 10 lines,
    then "and N more"), or "manifest changed", "version 0.1.0 → 0.2.0", or "publisher key
    changed";
  * the line: "This is a program that runs inside the game with your account's permissions."
* **Buttons:** "Allow", "Not now" (doesn't load; asks again at the next start) and "Never"
  (`state = "blocked"`). The default button and Escape are "Not now".
* **Several plugins:** one box each, in id order. "Not now for all" appears when there are more
  than three.
* **Launcher:** when the game starts from tsmod-launcher, the launcher runs the same check
  first (`tsmod-launcher` reads the same store and manifests) and shows a richer dialog with the
  full file list. Once it has approved, the in-game box doesn't appear.
* **Built (#297):** `ts_client::plugins` (`startup.rs` decides, `prompt.rs` asks). A box with no
  "Allow" is shown for a plugin that can't be approved as installed (§5.2 step 5); its "Never"
  still blocks. Plugins this client can't load whatever the player says (an unmet `[requires]
  port`, another `client_ext_api`, a library for another platform or architecture) are skipped
  with the reason and get no box. An "Allow" keeps an earlier approval's scope, so a rebuilt
  "this server only" plugin stays so, and records that approval's source (else `manual`).
  Without SDL2's message box the answer is "Not now", with a console line saying to approve in
  tsmod-launcher.
* **Clicks only, as far as a native box allows** (§7.3): Enter and Escape press "Not now", and an
  "Allow" within 2 seconds of the box appearing doesn't count: the box comes back with a line
  saying so. A message box can't disable a button or ignore the keyboard, so where the dialog
  lets Tab reach "Allow" (Windows, zenity) Space can press it. No server is connected at start,
  so that's an accidental-press risk only. Validation on fullscreen, Wine and HL25 is #312.
* **Test hook:** in a **debug build only**, `TSRS_DEV_PLUGIN_PROMPT=<id>=<answer>,...,*=<answer>`
  (`allow`, `later`, `later_all`, `never`) answers the boxes without showing them, and
  `TSRS_DEV_PLUGIN_PROMPT_LOG=<file>` (relative to the game directory) records each box's title,
  buttons, text and answer. An "Allow" for a box without that button answers "Not now". A release
  build ignores both and says so on the console. Environment only: no console command or server
  can reach it.

### 7.2 Server offer and update cards

These are drawn in game. Built (#299, `ts_client::plugins::offers::card`): a panel centred on the
screen, painted in `HUD_Redraw` after the HUD, the menus and the extensions (which get no `redraw`
while a card is up) with the engine's blended fills (`pfnFillRGBABlend`) and the console font,
with the virtual cursor (the view holds still, as with the team menu). Not the SDL card windows of
§13.5: those stop the engine's frames while open, and a player reading an offer on a server would
time out (#319). One card at a time; the rest queue.

* **Contents:**
  * the server's name and address;
  * the plugin's name, version, publisher, trust level and homepage;
  * the description;
  * download size and installed size;
  * `declares` in plain words ("starts programs: tsmod-webview-host", "uses the network",
    "stores data");
  * "Required by this server" and the server's note;
  * the native-code line from §7.1.
* **Buttons:**
  * "Install and allow";
  * "Allow on this server only": `scope = "server"`. The plugin loads only while connected to
    this address and stays loaded until the game exits (§9).
  * "Not now";
  * "Never from this server": the id is blocked for that address. Holding Shift turns it into
    "Never": the id is blocked everywhere.

### 7.3 Rules that keep a server from approving for the player

* **No console command approves.** The store is written only from a click in a dialog or from
  the launcher, so a server can't approve through `stufftext`, `alias` or binds.
* **Clicks only:**
  * the approve buttons take only a mouse click, never Enter, Space or a bound key;
  * they stay disabled for the first 2 seconds a card is shown, and after any card change, so a
    click aimed at something else doesn't land on them;
  * the default is always "Not now".
* **No flooding:** at most 4 offers per server per connection. A refused offer isn't shown again
  for the same id in that connection. "Never" stops it for good.
* **No cover:** while a card is open, the server can't draw over it. It's drawn last, above HUD
  and VGUI, and the webview overlay and other extensions get no `redraw` over it.
* **Clear source:** a card always says which server is asking.
* **Built (#299):** the delay is `plugins::click::ARM_DELAY`, which the start-up box (§7.1) and the
  relaunch cards (§13.5) share. A card's buttons take a left press and release on the same
  button, which reach the card only through the client's `IN_MouseEvent` (before the camera's and
  the menus' handling), never through a key, a bind or a console command; an approving button
  shows its countdown while disabled. Escape and Enter choose "Not now" and are swallowed;
  Shift (held) turns "Never from this server" into "Never (everywhere)". An open card is drawn
  over everything and suppresses the extensions' `redraw`. The e2e hook (§11) exists only in
  `--features e2e` builds. Real clicks, HL25 and Windows are #318.

## 8. Cvars and commands (client)

| cvar | default | meaning |
|---|---|---|
| `cl_plugins` | 1 | 0 loads no plugins at all, approved or not |
| `cl_plugins_download` | 1 | 0 ignores every server offer (answers nothing) |
| `cl_plugins_allow_unsigned` | 0 | 1 lets unsigned server packages reach the card |
| `cl_plugins_verify_log` | 0 | log each file hash at start |

* `cl_plugins_list`: installed plugins, version, state, scope, trust, loaded or not.
* `cl_plugins_menu`: a panel listing plugins. From it the player can revoke an approval, unblock,
  delete a plugin, and see every file and hash. Revoking takes effect at the next start (§9).
* `cl_plugins_verify`: re-hashes every plugin now and prints the result. It never approves.
* `tsrs_reconnect <address> [password]`: connects, first asking for the password in a card when
  the second word is the literal `password` (§13.5). The launcher's relaunch uses it. Any other
  second word is refused with the usage line, so a password typed as an argument is never used.
* `ts_dev_restart [restart | port <requirement> [<url>] | plugin <id@version>]` (dev aid):
  requests a restart and reconnect to the current server for that reason (§13.5), as op 5's
  card, an update of a loaded plugin and op 3 for an installed version do (#299, #302).

None of these can approve. A server that stuffs `cl_plugins 0` or `cl_plugins_menu` can, at
worst, disable plugins for the next start or open the panel. Both are harmless and visible. A
stuffed `tsrs_reconnect` does no more than a stuffed `connect`, and a stuffed `ts_dev_restart`
no more than a stuffed `quit` followed by a reconnect to the same server (a port update still
goes through the launcher's question).

* **Built (#297):** the four cvars (`FCVAR_ARCHIVE`, registered at `HUD_Init`), `cl_plugins_list`
  and `cl_plugins_verify`. The engines execute `config.cfg` only after `Initialize`, so start-up
  reads `cl_plugins` and `cl_plugins_verify_log` from the `config.cfg` line the engine archived
  (the last one wins; a value is on when it's a number other than 0, as `atof` sees it); a change
  takes effect at the next start. `cl_plugins_download` and `cl_plugins_allow_unsigned` are read
  when an offer comes (#299), with `cl_plugins_cancel` (port addition), which stops every
  download and approves nothing. `tsrs_reconnect` is #301.
* **Built (#311): `cl_plugins_menu`** (`ts_client::plugins::menu`: `model.rs` lists and changes,
  `view.rs` pages, `mod.rs` draws and routes input). The command takes no arguments and only
  opens the panel. The panel is an offer card (§7.2): the same layout, drawing, virtual cursor
  and click handling, painted in `HUD_Redraw` last, its clicks only through `IN_MouseEvent`;
  while it's open the extensions get no `redraw` and queued offer cards wait (a card already open
  stays on top and takes the input first). Escape closes it.
  * **The list:** every plugin start-up found (a package version or a hand-installed library),
    then every other id the store knows (a "Never", or an approval whose files are gone), by id,
    each a row with its name, version, approvals and what start-up decided (or "loaded").
  * **A plugin's details:** each store entry (state, scope and servers, trust, source, platform,
    time), what start-up decided, where it's installed, and every file with its SHA-256: the
    hashes start-up found, each marked when it differs from the approval of that version or isn't
    in it, then the approved files it didn't find. A plugin that isn't installed shows the
    approved hashes.
  * **Changes**, each behind a confirmation whose button is click-only and armed after the 2 s
    delay (§7.3); the details' buttons only open the confirmation:
    * "Revoke approval" removes the id's `allowed` entries (a "Never" stays). A loaded plugin
      stays loaded until the game exits (§9); one that hasn't loaded yet (a `scope = "server"`
      approval waiting for its server) won't load. While it's installed, the next start asks.
    * "Unblock" removes the id's `blocked` entries (an approval stays): the next start asks
      about it if it's installed, and servers can offer it again.
    * "Delete files" removes `cl_plugins/<id>/<version>/` (a real directory under
      `cl_plugins/<id>/`, never a link, a hidden directory or `staging/`) and that version's
      approval; a "Never" and `cl_plugins_data/<id>/` stay. Not while it's loaded (Windows can't
      remove a loaded library): revoke, and delete after the next start. Hand-installed
      libraries aren't deleted from here.
  * The result is a line at the top of the list and on the console. A store that couldn't be
    read at start (`approved.toml.bad`) can't be changed from the panel.
  * **Pages:** the list and the file list are cut to what fits the screen, at least one item a
    page, with "< Previous" / "Next >", Page Up / Page Down and the mouse wheel.

## 9. Loading

* **Approved at start:** loaded at `Initialize` through the client extension API, as specified
  in the webview spec §4.1.
* **Approved later** (a server offer or update): loaded at once, late. The port calls the
  extension's `init`, then `vid_init` if a map is loaded. Extensions must accept that order.
  `client_ext_api` must match exactly; on a mismatch, the reply is `failed api`.
* **`scope = "server"`:** loaded on connecting to a listed address. After that it stays loaded
  until the game exits, even on other servers, because unloading native code isn't safe. The
  card says so. The extension gets a `server_changed(address)` callback, and the webview plugin
  closes its server surface on it. Built (#297): the files are hashed again just before it's
  mapped, and the address is the net API's (`loopback` for a listen server).
* **Update while loaded:** the new version is installed and approved, but loads at the next
  start. The card says "restart to finish". Built (#299): the client answers `restart` and shows
  "Restart now / Later"; "Restart now" calls `relaunch::restart` for the plugin (§13.5). The
  loaded version's directory stays until the next start, which removes it before verifying.
* **Revoked while loaded:** stays loaded until exit.

## 10. Hashing and file rules (shared)

The pure parts go in a new crate, `crates/plugin-pkg`: no unsafe, shared by the client and
tsmod-launcher.
- manifest parsing and path rules (`manifest`, `path`)
- zip checks and unpacking (`package`), and writing packages for xtask (`build`)
- the store format (`store`)
- SHA-256 over files, in parallel (`hash`)
- the comparison in §5.2, which produces a `Change` list and a verdict (`verify`): load, prompt,
  blocked, or refuse (§5.2 step 5)
- signature and countersignature checks (`ed25519-dalek`; `sign`, `trust`)
- version changes and downgrades, and the update card's summary (`version`)

Its dependencies are kept to what the launcher already has (`zip` with deflate only, `sha2`,
`semver`, `toml`, `serde`, `base64`) plus `ed25519-dalek`.

## 11. Development and tests

* **Developers:** `cargo xtask plugin-approve <dir|tspkg>` writes a store entry for a local build
  on the developer's own machine. That's the same choice a player makes in a dialog, made at the
  terminal. The e2e roots use it, so runs don't stop at a prompt. Built (#297,
  `xtask/src/plugin_approve.rs`): `plugin-approve <tspkg | cl_plugins/<id>/<version> |
  cl_dlls/tsmod_<name><suffix>>... [--root R | --game-dir D] [--platform P]`. A `.tspkg` is
  installed first (needs `--root` or `--game-dir`), and what the client couldn't approve as it is
  is refused here too.
* **Unit tests (`plugin-pkg`):**
  * manifest and path rules (`..`, absolute paths, drive letters, symlinks, duplicate paths);
  * a zip with an extra or missing file;
  * every `Change` variant (changed, missing, extra, manifest, version, key, platform);
  * the store round trip and the `.bad` fallback;
  * signature, countersignature and key change;
  * downgrade detection.
* **Port unit tests:**
  * the offer state machine and the offer queue and limits;
  * the `TSPkg` encoding within 192 bytes: every op's round trip through the chunks, the largest
    bodies, bad input (`shared::protocol::tspkg`);
  * `tspkg` parsing and rate limits on the server, `plugin_offers.toml` parsing and the package
    checks (`ts_server::plugin_offers`).
  * the version policies (`any`, `same_minor`, `exact`, requirements) against `parse_version`;
  * `match` rules, `[[block]]` and `unlisted` decisions;
  * the grace period;
  * the refusal text fitting the reject buffer.
* **Server e2e (`cargo xtask tspkg`, validation.md §2.z2):** the server's side of 2 to 5 below,
  with the FWGS port client printing what it receives and its replies scripted: the offers and
  their checks, `tspkg` replies and the required-offer kick, op 5 and its kick, and no `TSPkg` to
  clients that don't hook it.
* **e2e (`cargo xtask e2e`, FWGS root with a dedicated server):**
  1. Approve a stub plugin with `plugin-approve`, start, and check it loads. Change one byte, add
     an extra file, delete a file; each must stop it loading, and the log must name the change.
     The prompt is answered by a test-only hook that records what it was shown.
     **Built (#297) in `cargo xtask client-plugins`** (FWGS listen server, `fwgs` and
     `fwgs-win`), not a separate `e2e` command: one start covers it, with an approved package that
     loads and starts its helper through `verified_spawn`, three approved packages tampered with
     afterwards (each refused, named, and asked about with no "Allow"), an unmet `[requires]
     port` (skipped, no box), a `scope = "server"` approval for the listen server (waits at start,
     loads on connecting), and new hand-installed libraries answered "Allow", "Never" and "Not
     now" by the §7.1 hook (the store checked afterwards).
  2. Serve a signed stub package from the root's FastDL and offer it. The test answers the card
     through a test-only hook, which is compiled only into `--features e2e` builds and is never a
     console command. Check the reply sequence `offered`, `downloading`, `loaded`, the installed
     files and the store hashes.
  3. Offer a package whose size or hash doesn't match the offer, a manifest whose signature
     fails, and an unsigned package. Each must answer `failed` or never reach the card, and leave
     nothing in `cl_plugins/`.

     **Built (#299) in `cargo xtask tspkg`** (each Linux root's dedicated server, the FWGS port
     client; validation.md §2.z2, scenarios 4–6), not a separate `e2e` command: xtask serves
     `ts/` as FastDL and can serve other bytes than the server checked; `TSRS_E2E_CARDS` answers
     the cards. Also covered: "Allow on this server only", an update of the loaded plugin
     (`restart`, the restart card through the fake launcher), `have`, `tspkg list`, the start's
     removal of the superseded version, and op 5's card through the fake launcher.
  4. An original TS 3.0 client on the same server must not receive `TSPkg` and must play
     normally.
  5. **Version matching:**
     * With `sv_client_version exact`, a client built with another version must be refused at
       connect, and the reason must name both versions. With `sv_client_require_port 1`, the
       original TS client must be refused too.
     * With a `[[block]]` on the stub plugin's version, `suspend` must stop its callbacks (it
       logs every call) and `kick` must drop the client with the reason.
     * A `match = "exact"` offer for a newer stub version must answer `restart`.
     * **Built (#302) in `cargo xtask tspkg`**, scenarios 7–10 (validation.md §2.z2): a block
       with `suspend` (the quiet stub prints its `suspend` and `resume` and counts callbacks in
       between: none), resumed on `disconnect`; a block with `kick`; a required `match =
       "exact"` offer of a newer version (update card, `restart`, one restart card, op 3, the
       kick at `sv_plugin_download_time`); `unlisted = "suspend"`. Scenario 3 adds `unlisted =
       "kick"` and a `manual:*` kick block: the older port client and TS 3.0's client still
       join.
     * **The plugins panel (#311) in `cargo xtask tspkg`**, scenarios 11–12: `cl_plugins_menu`
       with the same hook (`menu:<id>=revoke|unblock|delete`): revoking the loaded plugin
       (it stays loaded and installed, the store forgets it), unblocking a "Never from this
       server", and deleting a blocked, installed version (the block stays).
     * Restart and reconnect, with a fake launcher that records requests (`cargo xtask relaunch`,
       validation.md §2.r, with `crates/fake-launcher`'s `ts-launcher-fake`; the restart comes
       from `ts_dev_restart` until op 5 exists):
       * with `TSMOD_LAUNCHER_IPC` set, the client must send `relaunch` and quit;
       * with only `ts/launcher.toml`, it must start the fake launcher with the `relaunch`
         arguments;
       * on a passworded server, the password must appear nowhere in the request, argv, the
         environment or `.reconnect`. Only `password_required` is set, and the relaunched
         client must show the password card before connecting;
       * with neither, it must show the quit-and-inform card;
       * after a quit, the next start must show "Reconnect to …" and must not connect without a
         click.
  6. **The real relaunch, end to end.** Each starts a game connected to a test server, triggers
     a restart with op 5 or a `match = "exact"` offer, and passes only when the new game process
     is connected to the same server (the server log shows the join) and the old pid is gone:
     * FWGS Linux and FWGS win32 under Wine: through the real tsmod-launcher, running (IPC)
       and not running (`relaunch`), headless under Xvfb with its dialog answered by its test
       hook; and the self-relaunch with no launcher;
     * the update and downgrade paths: the launcher installs a release from a local release
       feed, `validate` passes, and the relaunched client reports the new `_tsrs` version;
     * a passworded server: the password card appears, and the test enters the password through
       the e2e hook;
     * Steam HL25 Linux, in `cargo xtask steam` (Steam status invisible while testing,
       validation.md §5): the `steam://run` self-relaunch, and the launcher path. HL25 Windows is
       manual until the Steam roots run under Wine.

     A failing platform keeps its self-relaunch disabled (§13.5 case 3).

     **Status, 2026-10-19:** e2e 5's restart scenarios run in `cargo xtask relaunch`
     (validation.md §2.r) and pass on FWGS Linux and FWGS win32 under Wine. They cover IPC, IPC
     without an answer then `relaunch`, `launcher.toml` only, the self-relaunch, quit and
     inform, the next start, and the passworded server with its card. The launcher in those
     runs is `ts-launcher-fake`. e2e 6 with the real tsmod-launcher and its update paths is
     #308. Steam HL25 is #307. Native Windows hasn't been run.

## 12. Wire compatibility

* `TSPkg` (and the webview spec's `TSExt`) are new user messages. TS 3.0's client doesn't hook
  them, and a GoldSrc client can fail on a user message it hasn't hooked ("UserMsg: Not Present
  on Client"). So the server sends them only to clients known to hook them: those whose userinfo
  carries `_tsrs` (`shared::protocol::port::USERINFO_KEY`), which the server already reads in
  `ClientConnect`, with a version at or above `port::TSPKG_SINCE` (3.0.9, the first release whose
  client hooks `TSPkg`). Port clients up to 3.0.8 carry `_tsrs` but don't hook it, and a `_tsrs`
  that isn't a version gets nothing either. Bots and HLTV get nothing. Builds between releases
  report the last release's version, so the dev aid `ts_dev_tspkg 1` (with `sv_cheats 1`) counts
  every port client with a readable version (`cargo xtask tspkg` uses it). The client takes
  `TSPkg` support from the port server's version (`SrvSett` keys 5–7), so neither side needs a
  new discovery channel.

  Original clients never get either message from the port's own code, and design.md §2's
  contract holds. `TSExt` is built (#294, client-ext-api.md §10): the plugin API's `ext_message`
  sends it only to clients that hook it, by `TSPkg`'s gate (the same release hooks both). A raw AMX Mod X `message_begin` of `TSExt` goes past the game
  library, so it is the AMX Mod X module's to guard (tsmod-amxx-rs#22).
* The registration order of TS's 64 messages doesn't change. New messages are appended after
  them in `shared::protocol::MESSAGES` (from `shared::protocol::TS_MESSAGES`): `TSExt` first, then
  `TSPkg`. Both sides find a message by name, so this order is the port's own choice; neither
  reaches the client extensions' `message_pre` (client-ext-api.md §11), nor does `SrvSett`.
* This client on an original TS server gets no offers, because nothing sends them.

## 13. Version matching

A server can refuse clients whose port version doesn't match its own, and can require exact or
compatible versions of plugins, or block plugin versions outright. This is for compatibility,
not anti-cheat: a modified client can report any version it likes, and the docs and cvar help
say so.

### 13.1 The port client's version

The client already reports its version in the `_tsrs` userinfo (§12), and the server already has
a refusal path in `ClientConnect` (`rules::client_connect`, which returns the engine's reject
reason). How a mismatch is handled depends on how old the client is:

* **Clients that understand `TSPkg`** (`_tsrs` at or above the first port release with it,
  `TSPKG_SINCE` in `shared::protocol::port`, 3.0.9; §12): the server lets them in (logging
  `[ts] version: let in <name> ... then kick: <reason>`) and, when they're put in the server,
  sends op 5 `port_version` (§13.3) with the refusal tag's requirement and
  `sv_client_version_url` (or `tsmod.org`). The client prints the line `[ts] version: this server
  needs The Specialists port <requirement>; get it via the TS launcher or <url>` and shows the
  update card (#299, once per connection): the requirement, this client's version and the URL,
  with "Restart to update" (click-only, armed late: it sends `tspkg tsrs <version> restart` and
  calls `relaunch::restart` for the port, which always goes through the launcher, and the launcher
  asks before it installs anything, §13.5) and "Not now" (`tspkg tsrs <version> denied`). The server kicks with the refusal text if the client hasn't left within
  `sv_plugin_check_time` (default 15 s; `ts_server::rules::client_version::decide`,
  `plugin_offers`). A map change reconnects the client, which starts this again.
  This path exists because `client.dll` never sees a `ClientConnect` refusal: the engine shows
  the reason, and the client can't act on it.
* **Older port clients, and TS 3.0's client** (with `sv_client_require_port 1`): refused at
  `ClientConnect` with the text below. They can't receive `TSPkg` (§12).

  Port clients up to 3.0.8, a `_tsrs` that isn't a version, and a requirement over op 5's 96
  bytes take this path.

| cvar (server, port addition) | default | meaning |
|---|---|---|
| `sv_client_version` | `any` | `any` (or empty); `same_minor` (same `major.minor`, the TS version: 3.0); `exact` (the server's own `VERSION`); or a semver requirement such as `>=3.0.8, <3.1` (Cargo's rules: a bare `3.0.8` means `^3.0.8`). Keywords are case-insensitive. A value that is none of these is logged at each connect (`[ts] version: sv_client_version "..." isn't ...`) and admits every version, as `any` does |
| `sv_client_require_port` | 0 | 1 also refuses clients without `_tsrs`: TS 3.0's original client, and port clients older than `_tsrs` |
| `sv_client_version_url` | `""` | shown in the refusal; empty shows `tsmod.org` (the site the 2.1 mode's refusal names) |

`sv_client_version` and `sv_client_require_port` are `FCVAR_SERVER`, so they appear in the
server's A2S rules. tsmod-launcher's server browser reads them and checks before the player
joins (§13.5). The server reads all three at each connect, so a change applies to the next one.

* **The version on the first connect.** GoldSrc puts a `FCVAR_USERINFO` cvar in the userinfo only
  once it is set, and the client can set its string only by a queued command, which runs after the
  command line's `+connect` (#196). So the client first sets `_tsrs` with `Cvar_SetValue` to the
  version as one number, `major * 1_000_000 + minor * 1_000 + patch` (`3000008` for 3.0.8;
  `shared::protocol::port::numeric_version`), which the engines write as `%d`, then to the string.
  The server reads both forms (`port::parse_userinfo_version`). Clients from before #303 send `1`
  first, which reads as an unknown version.
* **The refusal reason** is formatted as "This server needs The Specialists port 3.0.8 (yours:
  3.0.7). Update via the TS launcher or tsmod.org. [tsrs:=3.0.8]". The needed part is `3.0.8`
  (`exact`), `3.0.x` (`same_minor`), the requirement, or nothing (`any` with
  `sv_client_require_port 1`); "yours" is the client's version, `unknown`, or `TS 3.0` (no
  `_tsrs`, which reads "Get it via ..."). The tag carries the requirement in semver syntax for
  tools that read the engine log or the reject text: `*`, `~3.0`, `=3.0.8`, or the requirement as
  semver writes it (`>=3.0.8, <3.1`).
  * It's cut to 127 bytes: the engines' `szRejectReason` is 128 with the NUL (FWGS `SV_New_f`,
    ReHLDS `SV_ConnectClient_internal`, which reimplements HLDS's). The body is cut at a character
    boundary and the tag is kept whole. With the default URL, `any`, `same_minor` and `exact` fit
    uncut for versions up to 3.10.10; a long URL or requirement loses the end of the body.
  * `;`, `"` and control characters become spaces: ReHLDS and HLDS also send the reason to the
    client as `echo <reason>` console text, where they would start another command.
  * The server logs `[ts] version: refused <name> (<client>): <reason>`.
* **Bots and HLTV** are never checked. A `_tsrs` value that isn't a version counts as a mismatch,
  unless the policy is `any`.
* **Gameplay rulesets:** a ruleset's own client check (`gameplay::refuse_client`, the TS 2.1
  mode's) runs first and keeps its own reason. In 2.1 mode TS 3.0's client gets the 2.1 refusal
  whatever `sv_client_require_port` says, and port clients then face the version check.
* **The plugin API** has `client_port_version(player, buf, len)` since version 8 (#304,
  plugin-api.md §13), so AMX Mod X plugins can apply a policy of their own: the version as
  `major.minor.patch`, 0 for TS 3.0's client, bots and HLTV, `UNSUPPORTED` for an unreadable value.
* **The client side:**
  * The client compares the server's version (`SrvSett` keys 5–7, complete at key 7) with its own
    and prints the difference in the console (`[ts] version: this server runs The Specialists
    port 3.0.8, this client is 3.0.7`) at each map, without refusing. TS 3.0's server sends no
    key 5, so nothing is compared there.
  * With `cl_version_strict 1` (default 0, archived), it refuses to stay on a server whose version
    differs from its own (same rules as `exact`): it prints the line with `cl_version_strict 1
    disconnects` and runs `disconnect`. For players who want to know they're on matching code.

### 13.2 Plugin versions

Each `[[offer]]` in `plugin_offers.toml` gets a `match` rule, and a new `[[block]]` table lists
plugins that may not be used on the server:

```toml
[[offer]]
id = "webview"
version = "0.2.0"
required = true
match = "compatible"      # exact | compatible (the default) | any | a semver requirement (">=0.2.0, <0.4")
# ... url, sha256, size, note as in §6.1

[[block]]
id = "webview"
versions = "<0.2.0"       # semver requirement; "*" blocks every version
action = "suspend"        # suspend (default) | kick
reason = "0.1.x breaks the vote page"

[policy]
unlisted = "allow"        # allow (default) | suspend | kick: plugins in neither table
```

* `compatible` means the same major version for `1.0.0` and later, and the same minor version
  before `1.0.0`, as Cargo's caret rule does. `exact` means the offered version only. Without
  `match`, an offer is `compatible`.
* The server parses and keeps `match`, `[[block]]` (`id` a plugin id, `manual:<name>` or
  `manual:*`; `versions` a semver requirement up to 96 bytes; `reason` up to 64 bytes) and
  `[policy]`, and records each client's `tspkg list` (#298, `plugin_offers::config`), and
  enforces them (§13.3, #302, `plugin_offers::enforce`).
* **What the client reports:** after the first `TSPkg` from the server, the client sends
  `tspkg list` naming every loaded plugin and version, chunked like other commands under 128
  bytes. A client that sends nothing within `sv_plugin_check_time` (default 15 s) is treated as
  having no plugins. Built (#302): the client also sends it as soon as the server's version
  (`SrvSett` 5–7) is one that understands `tspkg` (`port::TSPKG_SINCE` on), so a server with
  blocks or a policy but nothing to offer learns it too; the server accepts a list from any
  client that hooks `TSPkg`. It goes again at every map (the map change's reconnect makes the
  server forget it). A list still incomplete at the end of the grace period counts as what came.
  The version is the one the plugin's approval was verified at; a version that isn't semver
  counts as `0.0.0` on the server, so a client can't dodge a `<x` block with a token.
* **Hand-installed libraries** report as `manual:<name>@0.0.0`. Only `unlisted` and an explicit
  `[[block]] id = "manual:*"` (or `manual:<name>`) apply to them.

### 13.3 Enforcement

For each loaded plugin the client reports, and each required offer, the server decides:

| Situation | What happens |
|---|---|
| A required plugin isn't installed | the normal offer (§6.3); refusing it follows `sv_plugin_required_kick` |
| A required plugin is loaded, but its version doesn't satisfy `match` | an update offer for the server's version (§6.4); the client also gets op 3 `require` |
| A loaded plugin satisfies `match` | nothing |
| A loaded plugin matches a `[[block]]`, or is unlisted with `unlisted` ≠ `allow` | op 4 `block` with its action |

New `TSPkg` ops (same framing as §6.2):

```
op 3 require   id, version requirement (≤ 96), flags (1 required)
op 4 block     id (≤ 40: a plugin id, manual:<name> or manual:*), version requirement,
               byte action (1 suspend, 2 kick), string reason (≤ 64)
op 5 port_version   id ("tsrs"), version requirement (≤ 96), string url (≤ 255)
```

* **Suspend:** the port stops calling the plugin's callbacks while the player is on this server:
  `redraw`, `key_event`, `user_message`, `usercmd`, `message_pre`, `command_pre`, and the rest.
  The port controls all of them, so a suspended plugin can't draw, intercept or receive
  messages.
  * First it calls a new `suspend(reason)` callback, where the plugin must hide its surfaces and
    pause its helpers. The webview plugin closes both surfaces and lets the host idle.
  * On disconnect it calls `resume()` and dispatches normally again.
  * The client answers `tspkg <id> <version> suspended`. A client that doesn't answer within
    `sv_plugin_check_time` is kicked.
* **Kick:** the server drops the player with the block's reason, naming the plugin and version.
  The client shows the reason in its disconnect dialog.
* **Getting the right version:** a loaded plugin can't be swapped while the game runs (§9), so
  an update for a loaded plugin answers `restart`. A port update always needs a restart too.
  Both go through restart and reconnect (§13.5).
* **Grace period:** the server applies none of this until `sv_plugin_check_time` has passed or
  the client has answered every offer, so a player who is approving and downloading isn't
  kicked mid-download. Required-plugin kicks wait for the download to finish, up to
  `sv_plugin_download_time` (default 300 s).
* **The plugin API** gets the event `client_plugin_list(player, plugins, count)` (when the
  list is complete, once per connection), and `client_plugin_version(player, id, buf, len)` (the
  answer `loaded`, else the list, else `have`). Built in version 8 (#321, plugin-api.md §13).
* **Logs:** the server logs each refusal, suspension and kick with the player, the plugin, the
  version and the rule that matched.
* **Built (#302).** Server: `ts_server::plugin_offers::enforce` (engine-free, unit-tested),
  stepped every `StartFrame` for each client the offers went to:
  * **Who:** only clients that hook `TSPkg` (§12). TS 3.0's client and port clients before
    `TSPKG_SINCE` can't be told and can't run client plugins, so nothing here applies to them,
    whatever the policy; `sv_client_require_port` (§13.1) is what keeps them out. Bots and HLTV
    are never checked.
  * **Grace:** it starts when the offers go out (the first `PlayerPostThink`, §6.1) and ends at
    `sv_plugin_check_time`, or earlier once the list is complete and every offer has an answer
    other than `offered` or `downloading`.
  * **Judging** each listed plugin, once: the first `[[block]]` (file order) whose id and
    `versions` match wins, even over an offer of the same id; else an offer of the id accepts its
    version through `match`, or sends op 3 (`=v` for `exact`, `^v` for `compatible`, `*`, or the
    requirement) with the `required` flag when the offer has it; else `unlisted`. Op 3 for an
    offer that isn't required only tells the client; nothing is enforced. An unlisted plugin gets
    op 4 with the requirement `*` and the reason "not allowed on this server". Op 4 names the
    plugin as listed (`manual:dev`, not the block's `manual:*`).
  * **Suspend** waits `sv_plugin_check_time` for `tspkg <id> <version> suspended`, then kicks
    ("..., and it wasn't suspended."). **Kick** sends op 4 and kicks at once: "This server blocks
    the client plugin <id> <version>: <reason>". Block kicks don't need
    `sv_plugin_required_kick`; the block is the admin's explicit choice.
  * **Required offers** kick at `sv_plugin_download_time` after the offers went out, with
    `sv_plugin_required_kick 1`, unless the plugin is loaded at a version `match` accepts (an
    answer `loaded`, else the list's version, else `have`). A refusal still kicks at once (§6.1).
    So a player who restarts for an update has the download time to come back, and one who
    chooses "Later" on the restart card is kicked then.
  * **Logs:** `[ts] tspkg: <player>: <id> <version>: <rule>: suspend|kick|require <req>`, the op
    sent (`block webview <0.2.0 (suspend) to <player>`), the answer, and `kicking <player>: ...`.
  * The plugin API's `client_plugin_list` and `client_plugin_version`, with
    `offer_client_plugin` and `client_plugin_state`, are in version 8 (#321).

  Client: `ts_client::plugins::offers::enforce` decides, `offers` acts:
  * **Op 4 `suspend`:** every loaded plugin the block names (id, `manual:<name>` or `manual:*`,
    at a version the requirement accepts) is suspended through `ext::suspend` with the block's
    reason (or "blocked by this server") and answered `suspended`, again if it already was (after
    a map change). A block naming nothing loaded is printed only. Leaving the server (a new
    address or a disconnect) resumes it (`ext`'s server change), with a console line.
  * **Op 4 `kick`:** printed ("this server doesn't allow <id> <version> and disconnects players
    who use it"); the server's kick follows.
  * **Op 3:** nothing when the loaded version fits or the plugin isn't loaded, or while an offer
    of it is being handled (its update card leads to `restart` and the restart card, §9). When it
    doesn't fit and an approved version that does is installed for this server (the player chose
    "Later" on a restart card before), the client answers `tspkg <id> <version> restart "needs
    <req>"` and shows the restart card ("Restart now" calls `relaunch::restart(Plugin)`), once
    per connection and plugin. Otherwise it prints that the server's offer updates it.

### 13.4 Client cvars

| cvar | default | meaning |
|---|---|---|
| `cl_version_strict` | 0 | 1 disconnects from servers whose port version differs from the client's (`FCVAR_ARCHIVE`; §13.1) |

`cl_plugins_list` shows, for the current server, which loaded plugins are suspended and why.
Built (#302): "loaded, suspended by this server (<reason>)". `cl_version_strict` is #300's.

### 13.5 Restart, reconnect and updates

When the game must restart to get onto a server, because of a plugin update (§13.3) or a port
version the server needs (op 5), the client uses the first of these that's available:

1. **The launcher is running:** the client tells it directly, over IPC.
2. **The launcher is installed but not running:** the client starts it with relaunch arguments.
3. **No launcher, but the game can relaunch itself:** it does.
4. **None of these:** the client offers to quit, and tells the player how to come back.

Updating the port itself always needs the launcher (case 1 or 2), and the launcher always asks
first. Without a launcher, the card says where to get the version and offers to quit. So a
`port` restart skips case 3: relaunching the same build would only be refused again.

The port side is `ts_client::relaunch` (issue #301): `relaunch::restart(reason)` is the entry
point for op 5 and plugin restarts: op 5's card and an update of a loaded plugin call it (#299),
as the dev aid `ts_dev_restart` (§8) and op 3's restart card (#302) do. It reads the server's
address from the engine's net API (`Status`,
`AdrToString`; a listen server's `loopback` can't be restarted into) and its name from the HUD's
`ServerName`.

**The version refusal (§13.1) doesn't trigger a restart.** `client.dll` never sees a
`ClientConnect` refusal: the engine shows the reason (with its `[tsrs:<req>]` tag) in its own
dialog and console. Only op 5, which a client that understands `TSPkg` gets instead of the
refusal, starts this path. The tag is for tools that read the reason, such as the launcher's
server browser (tsmod-launcher #6).

#### Finding the launcher

* **Launched by the launcher:** tsmod-launcher sets these environment variables on the game
  process, and the client reads them when a restart is needed:
  * `TSMOD_LAUNCHER`: the launcher's executable;
  * `TSMOD_LAUNCHER_INSTANCE`: its installation id (as in `ts-launcher list`). Required with
    `TSMOD_LAUNCHER`: without it the variable is ignored and `launcher.toml` is tried;
  * `TSMOD_LAUNCHER_IPC`: the IPC endpoint, while the launcher is running.
* **Started some other way** (from Steam directly, or a desktop shortcut): the launcher writes
  `<gamedir>/launcher.toml` (`ts/launcher.toml`) into every installation it manages. It's
  rewritten at every launch and install:

  ```toml
  format = 1
  launcher = "/home/me/.local/bin/ts-launcher"   # the launcher's executable, absolute
  launcher_version = "0.13.0"
  instance = "ab12"                              # the installation id: [A-Za-z0-9_.-]{1,64}
  root = "/home/me/games/ts-xash"                # the installation root: the directory the engine
                                                 # runs in (the one holding ts/), absolute
  ```

* **Checks** (`relaunch::launcher`): the client starts a launcher only if
  * its executable exists, is a file, and its file name starts with `ts-launcher` (any case:
    `ts-launcher`, `ts-launcher.exe`, `ts-launcher-v0.13.0-linux-x86_64.bin`);
  * the executable isn't inside the game directory, where a server's downloads land (FWGS
    downloads only three-letter extensions, so `launcher.toml` itself can't be downloaded,
    but a planted file mustn't become a program the client starts);
  * on Unix, it has an execute bit;
  * for `launcher.toml`: `format` is 1, `instance` is an id, and `root` is the directory the
    game runs in (both canonicalized).

  A stale or foreign file is ignored, as if there were no launcher; the console says why
  (`[ts] relaunch: launcher.toml ignored: ...`). The environment's launcher is checked the same
  way, except for `root`.

#### 1. Launcher running: IPC

* **The endpoint** is `TSMOD_LAUNCHER_IPC`'s value as it is; the client never guesses one.
  The launcher creates it:
  * Linux: a Unix socket, `$XDG_RUNTIME_DIR/ts-launcher-<uid>.sock` (falling back to
    `/tmp/ts-launcher-<uid>/sock`, mode 0700). The client connects only to an absolute path
    whose owner is the client's own user;
  * Windows: the named pipe `\\.\pipe\ts-launcher-<user SID>`, accessible only to the current
    user. The client opens it as a file (byte mode). Under Wine the whole chain is Windows
    programs (the game, the launcher's Windows build), so the pid is Wine's.
* **The request:** one connection per request, one JSON object on one line (UTF-8, ending in
  `\n`), protocol version 1. Keys in this order, optional ones left out when they don't apply:

  ```json
  {"op":"relaunch","v":1,"instance":"ab12","pid":41733,"connect":"203.0.113.5:27015",
   "password_required":true,"reason":"port","port_version":">=3.0.8, <3.1",
   "url":"https://tsmod.org","server_name":"Bob's TS","client_version":"3.0.8"}
  ```

  | key | | meaning |
  |---|---|---|
  | `op` | always | `"relaunch"` |
  | `v` | always | 1; a launcher refuses versions it doesn't know |
  | `instance` | when known | the installation id (`TSMOD_LAUNCHER_INSTANCE` or `launcher.toml`) |
  | `pid` | always | the game process to wait for |
  | `connect` | always | `host:port`: host name or IPv4 (`[A-Za-z0-9.-]`, not starting with `-`) or `[IPv6]`, port 1–65535, ≤ 64 bytes |
  | `password_required` | always | `true` when the `password` cvar was set; never the password |
  | `reason` | always | `"restart"` (nothing to update), `"port"` or `"plugin"` |
  | `port_version` | `port` | a semver requirement, ≤ 48 bytes (op 5's) |
  | `url` | `port`, when the server gave one | where the server says to get the version, ≤ 96 bytes; shown only |
  | `plugin` | `plugin` | `id@version`, shown only |
  | `server_name` | when known | ≤ 64 bytes, control characters removed; shown only |
  | `client_version` | always | the running port client's version |

* **The handshake:**
  1. The launcher answers one line, `{"ok":true}`, and the client quits the game at once
     (`quit`). Anything else (`{"ok":false,"error":"..."}`, no answer within 2 s, a closed
     connection) and the client falls back to case 2.
  2. The launcher waits for that pid to exit, up to 30 s. It has the `Child` handle when it
     started the game itself.
  3. If the request is for a port update, it asks the player (below).
  4. It relaunches the installation with `+connect <address>`, or with
     `+tsrs_reconnect <address> password` when `password_required` is set (see "Passwords"
     below). This is the launcher's existing `launch(install, opts, extra)`.

#### 2. Launcher installed, not running: relaunch arguments

The client starts the launcher detached (its own process group; `DETACHED_PROCESS` on
Windows; no console output), then quits:

```
ts-launcher relaunch --instance ID --wait-pid PID --connect ADDR [--password-required]
                     --reason restart|port|plugin [--port-version REQ] [--url URL]
                     [--plugin ID@VERSION] [--server-name NAME]
```

* The arguments come in this order, each value as its own argument (no shell). Their rules are
  the IPC request's. The launcher should refuse arguments it doesn't know.
* `relaunch` is a new launcher subcommand. It opens the launcher's window on a "Reconnecting to
  <server>" view that waits for the game to exit, then runs the same steps as the IPC request.
  When the launcher already has an instance open, the new process hands the request to it over
  IPC and exits.
* The launcher's environment is the game's, unchanged: the client adds nothing to it.

#### Passwords

The password is asked for again after the relaunch; it's never passed on.

* **The client never reads the `password` cvar to send it anywhere.** The IPC request, the
  `relaunch` arguments, the environment and `.reconnect` carry only `password_required`: whether
  the `password` cvar was set when the restart was needed.
* **After the relaunch:** the game starts with `+tsrs_reconnect <address> password` instead of
  `+connect`. `tsrs_reconnect` is a port-only client command. With `password`, it shows a card
  with the server's address (the name isn't on the command line) and a masked password field
  ("Connect" / "Cancel"), sets the `password` cvar from it (`password "<typed>"` through the
  client command buffer, which doesn't echo) and connects. Without it, it's the same as
  `connect`. It can do nothing a server's `connect` stufftext can't already do.
* **The field** takes printable ASCII except `"`, up to 63 characters, and shows a `*` for
  each. "Connect" needs a click and a non-empty password.
* **The launcher doesn't ask:** it would have to pass the password on the game's command line,
  where other users can read it.
* **If the password is wrong,** the engine's refusal ("Bad server password") shows as usual, and
  the player can reconnect from the console or the server browser.
* The quit-and-inform card (case 4) and the next start's "Reconnect to <server>?" card ask the
  same way: that card has the password field when `.reconnect` says one is needed.

#### Updating the port through the launcher

When the request is for a port update, the launcher shows:

* the server's name and address;
* the required version, and the installed one;
* the release it will install: the newest port release that satisfies the requirement, from the
  same release source it already uses (`--port-version`). Nothing the server sends selects a
  download location; the server gives only a version requirement.

The choices are:

* **"Update and reconnect":** installs that release into the installation, as
  `install --port --port-version TAG` does, validates it (`validate`), then relaunches with
  `+connect`.
* **"Reconnect without updating":** the server will refuse again; the launcher says so.
* **"Cancel":** doesn't start the game.

Notes:

* **Downgrades:** when only an older release satisfies the requirement, the launcher offers to
  downgrade. The dialog says so: "This server runs an older version (3.0.6). Downgrading changes
  this installation for every server; you can update again later." The choices are:
  * **"Downgrade and reconnect":** installs the older release the same way, validates it and
    relaunches. The launcher records the version it replaced, so its main view offers "Update to
    3.0.9 again", and a newer server's requirement brings the normal update dialog.
  * **"Reconnect without changing"** and **"Cancel",** as above.

  The default button is Cancel, so a stray Enter doesn't downgrade. Approved plugins stay in
  `cl_plugins/`; a plugin whose `[requires] port` (§3.2) the older port doesn't meet is skipped at start, with
  the reason, until the port is updated again. The launcher doesn't keep a second installation
  per server.
* **Plugin updates** need no launcher update step. The new plugin version was already approved
  in game, and the launcher's start-up check (§7.1) finds it approved.
* **Before joining:** in the launcher's own server browser, the launcher compares a server's
  version (A2S info, "The Specialists v3.0.8") and its `sv_client_version` rule with the
  installed port. It offers the same update dialog before starting the game, so the round trip
  through the game isn't needed.

#### 3. No launcher: self-relaunch

* **FWGS:** the client re-executes the engine binary (`current_exe`) with its original
  parameters, then the connect command (`+connect <address>`, or `+tsrs_reconnect <address>
  password`), and quits. It reads the arguments from `/proc/self/cmdline` on Linux and
  `GetCommandLineW` (std's parsing) on Windows. Every console command of the original command
  line is dropped (`+name` and the words after it up to the next `+` or `-` word: `+connect`,
  `+exec`, `+map`, `+password`); the engine parameters (`-game ts`, `-windowed`, ...) are
  kept. Repeating a command could run the same script, restart included, again, and a
  `+password` would pass the password on.
* **Steam HL25:** the client opens `steam://run/70//-game%20ts%20+connect%20<address>/` (or
  the `+tsrs_reconnect ... password` form), through `xdg-open` or `rundll32
  url.dll,FileProtocolHandler`, then quits. Steam starts the game again once it has exited.
* Each is enabled per engine and platform only once §11's relaunch tests pass there
  (`SELF_RELAUNCH_VERIFIED` in `crates/client/src/relaunch/plan.rs`). On a platform not in it,
  or where a test fails, the client goes on to case 4. The FWGS test is `cargo xtask relaunch`
  (validation.md §2.r). Its e2e build forces the self-relaunch on with
  `TSRS_E2E_SELF_RELAUNCH=1`, so a platform can be verified before it is listed.
  * **FWGS Linux: on.** The `self` scenario passed on 2026-10-19 (#332).
  * **FWGS Windows: on, verified under Wine only.** The `self` scenario passed on `fwgs-win`
    (FWGS 0.21 win32 under Wine), the same day. So the re-exec rebuilds a working command line
    from Wine's `GetCommandLineW`. The client can't tell Wine from native Windows, and
    `Platform::FwgsWindows` is one value for both, so native Windows FWGS gets the
    self-relaunch on the strength of the Wine run. It hasn't been run there.
  * **Steam HL25 (Linux and Windows): off.** The test belongs in `cargo xtask steam` and doesn't
    exist yet (#307). It has to prove that Steam starts the game again while the old process is
    still exiting.

#### 4. Nothing else: quit and inform

* **The card says:** "Restart the game and reconnect to <server name> (<address>)", and for a
  port update also "This server needs version <requirement>; get it from <url> or the TS
  launcher".
* **The address:** it's copied to the clipboard (`SDL_SetClipboardText`).
* **The buttons:** "Quit now" and "Stay" (stay disconnected, or on the server until it kicks).
* **Next start:** the client writes `<gamedir>/cl_plugins_data/.reconnect` when it shows the
  card (whichever button is chosen, so a player who stays and quits later is offered it too):

  ```toml
  format = 1
  address = "203.0.113.5:27015"
  server_name = "Bob's TS"
  password_required = true     # never the password
  time = 1792300000            # Unix seconds
  ```

  At the next start, 30 frames in (after the command line's own `+connect` or
  `+tsrs_reconnect`), the client reads it once and deletes it. Within 10 minutes of `time`, and
  unless the game is already connecting or a `tsrs_reconnect` ran, it shows "Reconnect to
  <server>?", a click card under §7.3's rules, with the password field when
  `password_required`. Nothing connects without that click.

#### The cards

The main menu has no HUD to draw on, so the three cards (quit and inform, "Reconnect to ...",
the password card) are small windows of their own: an SDL2 window from the library the engine
has loaded (`goldsrc::platform::sdl::Window`), drawn with filled rectangles on its surface
(SDL's software path, which never touches the engine's GL context) and the public-domain
font8x8 glyphs. They are modal: the engine's frames stop while one is up, so a card left open
for long while connected can time the connection out. The engine window's events during that
time are dropped. Every card follows §7.3: the approving button ("Quit now", "Reconnect",
"Connect") takes only a mouse click and is disabled for 2 s; Enter does nothing; Escape and
closing the window choose the other button. Without SDL2, or when the window can't open, the
card is printed to the console and answered as "Stay" / "Cancel". Over a fullscreen game the
window is raised and always on top; whether every window manager shows it there is part of
open question 3.

#### Safety

* **The IPC endpoint and `relaunch` only ever start the game, connect, or ask.** Any local
  program could send a request, so the launcher treats every update as a question for the
  player, never as an instruction.
* **What a request can contain:**
  * the address must be a `host:port`;
  * the requirement must parse as semver;
  * the plugin field is only shown, never acted on;
  * releases come only from the launcher's configured source.
* **A server can't make the launcher download anything:** it can only state a version
  requirement, and the player still has to choose "Update".

## 14. Open questions

1. ~~`TSPkg` can exceed 192 bytes with a long URL and note. Should it chunk, as the webview
   `TSExt` framing does, or move the note into the manifest?~~ Resolved (#298): it chunks (§6.2),
   which also lets the server send the full URL.
2. **The TLS stack:** `ureq` and `rustls` add about 1.5–2 MB to each `client.dll`. The
   alternative is the engine's own download (`precache_generic` plus `sv_downloadurl`), which
   accepts `.tspkg` names. But it runs at connect time, blocks joining for large packages, and
   can't show a card before downloading.
3. Does `SDL_ShowMessageBox` show correctly over a fullscreen window at `Initialize`, on both
   engines and on Wine? If not, the start-up prompt waits for the first frame and uses the VGUI
   card, and plugins awaiting approval load late (§9). Shown correctly over windowed FWGS Linux
   under Xvfb (2026-10-18); the rest is #312.
4. The countersigning key: who holds it, and how a publisher asks for verification. Probably a
   release-repo workflow in the-specialists-mod.
5. Libraries a helper process loads on its own (`libcef.so`, `libcef.dll`) are hashed at start,
   but they could be swapped between that check and the helper loading them. Should
   `verified_spawn` re-hash all of a declared process's files, at a cost of about 0.2 s per
   start of the helper?
6. Should the launcher be able to install packages from the mod's own release feed, with the
   same cards, so players needn't wait for a server to offer them?

## 15. Milestones (port issues)

1. `plugin-pkg` crate: manifest, store, hashing, comparison, signatures, with the unit tests.
2. Client: the loader with start-up verification and the SDL prompt, hand-installed libraries,
   `cl_plugins_*` commands, `xtask plugin-approve`. Depends on the client extension API.
3. Server: `TSPkg` (sent only to `_tsrs` clients), `plugin_offers.toml`, `sv_plugin_offer`, `tspkg` replies
   and the plugin API additions. Built (#298); the plugin API additions are in version 8
   (`client_port_version` #304, the offers and events #321).
4. Client: the offer flow, downloads, the in-game cards and their safeguards (§7.3), late
   loading, e2e. Built (#299).
5. tsmod-launcher: the same verification and approval UI, using `plugin-pkg`. Also:
   * the `TSMOD_LAUNCHER*` environment variables and `ts/launcher.toml`;
   * the IPC endpoint and the `relaunch` subcommand;
   * the update-and-reconnect dialog, with downgrade and "update again";
   * the server browser's version check.

   These are launcher issues, filed in `the-specialists-mod/tsmod-launcher`.
6. Version matching:
   * the mod version: `sv_client_version`, `sv_client_require_port`, the formatted refusal
     with its tag, `cl_version_strict` and `client_port_version`. This part needs no plugins,
     and can ship first. Op 5 and the update card come with `TSPkg`.
   * restart and reconnect (§13.5): finding the launcher, the IPC request and `relaunch`
     arguments, `tsrs_reconnect` and its password card, self-relaunch where its tests pass, the
     quit-and-inform card and `.reconnect`, and the relaunch e2e (§11 e2e 6) on every root.
   * plugins: `match`, `[[block]]` and `[policy]`, `tspkg list`, ops 3 and 4, suspend and
     resume, restart and reconnect, and the plugin API events. Built (#302); the plugin API
     events are in version 8 (#321).

## 16. Decisions made building milestone 2 (#297)

Where this specification was silent or ambiguous, the loader (`crates/client/src/plugins/`)
chose, and the sections above now say so. In short (issue #313):

* **`cl_plugins` at `Initialize`:** read from `config.cfg` (§8), since the engine runs it later.
* **What gets a box:** new and changed plugins, and those that can't be approved as installed
  (without "Allow"). Plugins this client can't load anyway (`[requires] port`, `client_ext_api`,
  a library for another platform) are skipped with the reason and no box (§3.2, §7.1).
* **"Not now for all"** appears when more than three boxes are to be shown.
* **An "Allow" at start** keeps the earlier approval's scope and source (§7.1).
* **Clicks only** is as far as a native box allows (§7.1, #312).
* **The prompt hook** is an environment variable in debug builds, not an SDL stub (§7.1, §11).
* **`verified_spawn`'s "listed"** is any approved file of the plugin (§5.2).
* **e2e 1** runs in `cargo xtask client-plugins` on a listen server (§11).

## 17. Decisions made building milestone 4 (#299)

Where this specification was silent or contradicted itself, the offer flow
(`crates/client/src/plugins/offers/`) chose, and the sections above now say so (issue #319):

* **The cards are drawn in game** in `HUD_Redraw`, last, not as VGUI1 (the port has none) or the
  SDL windows of §13.5 (they stop the frames, so a server would time the player out) (§7.2).
* **`have`** compares the manifest's hash, which the store keeps, not the package's (§6.3).
* **Progress** is a line at the top of the screen, without a cursor; Cancel is
  `cl_plugins_cancel`, a disconnect or a withdrawal (§6.3).
* **An update of a loaded plugin** answers `restart` and offers "Restart now" through
  `relaunch::restart` (§9).
* **Op 5's card** restarts through the launcher and answers `tspkg tsrs <version> restart` or
  `denied` (§13.1).
* **`offered`** goes when the card is queued; `cl_plugins 0` ignores offers like
  `cl_plugins_download 0` (§6.3).
* **"Allow on this server only"** of the same files keeps the servers allowed before (§6.3).
* **Superseded versions** go at approval unless loaded, else at the next start (§4, §9).
* **e2e 2–3** run in `cargo xtask tspkg` with a FastDL stand-in and the `--features e2e` card hook
  (§11).
* **Offers reach FWGS clients** only once spawned, so the server sends them at the first
  `PlayerPostThink` (§6.1, #317).

## 18. Decisions made building enforcement (#302)

Where §13.2 and §13.3 were silent, the enforcement (`ts_server::plugin_offers::enforce`,
`ts_client::plugins::offers::enforce`) chose, and the sections above now say so (issue #322):

* **The list without a `TSPkg`:** the client also sends `tspkg list` once the server's version
  says it understands `tspkg`, and the server takes a list from any client that hooks `TSPkg`;
  otherwise a server with blocks or a policy but no offers would never learn it (§13.2).
* **Again at every map:** the server's per-client state starts again at the map change's
  reconnect, so the client sends the list at each map, not once per address (#323) (§13.2).
* **Who is checked:** only clients that hook `TSPkg`. TS 3.0's client and older port clients
  can't run client plugins or be told, so no policy reaches them; `sv_client_require_port` does
  (§13.3).
* **Precedence:** the first matching `[[block]]` wins, over an offer of the same id too; then the
  offer's `match`; then `unlisted` (§13.3).
* **Grace:** "answered every offer" means an answer other than `offered` or `downloading`, with
  the list complete; a list incomplete at the end counts as what came (§13.3).
* **Kicks:** block kicks are immediate and need no cvar; required-offer kicks need
  `sv_plugin_required_kick 1` and wait for `sv_plugin_download_time` from the offers going out;
  a suspend not answered within `sv_plugin_check_time` kicks (§13.3).
* **Non-required mismatches** get op 3 without the `required` flag, and nothing more (§13.3).
* **Op 4's id** is the plugin as listed; op 4 for an unlisted plugin carries `*` and "not allowed
  on this server" (§13.3).
* **Versions** that aren't semver count as `0.0.0` on the server (§13.2).
* **Op 3 with an installed version** answers `restart` and shows the restart card, once per
  connection; while an offer of the plugin is being handled, its own card does that (§13.3).
* **e2e 5's blocks and `match`** run in `cargo xtask tspkg`, scenarios 7–10 (§11).

## 19. Decisions made building `cl_plugins_menu` (#311)

Where §8 was silent, the panel (`crates/client/src/plugins/menu/`) chose, and §8 now says so
(issue #330):

* **Drawn as an offer card**, in game, not as an SDL window (those stop the frames) or VGUI (the
  port has none); offer cards queue behind it, and a card already open stays above it.
* **Confirmations:** every change has one, and only its button writes, armed like an approving
  button (§7.3); the panel's other buttons are navigation.
* **Revoke and unblock are separate:** revoke removes the `allowed` entries and unblock the
  `blocked` ones (`Store::remove_state`), rather than `Store::remove` taking both.
* **Revoke stops a plugin that hasn't loaded yet** (a server-scoped approval waiting for its
  server), as well as at the next start.
* **Delete** removes one version's directory and its approval, keeps a "Never", and is refused
  while the plugin is loaded; hand-installed libraries aren't deleted.
* **Files** are those start-up hashed (marked against the approval), so a changed file shows its
  new hash; an uninstalled plugin shows its approved hashes.
* **e2e:** the `TSRS_E2E_CARDS` hook answers the panel too (`menu:<id>=revoke|unblock|delete`),
  in `cargo xtask tspkg` scenarios 11–12 (§11).
