# Client plugins: packages, downloads and approval

Status: draft specification, 2026-10-17. Built so far: the pure parts (§3, §5, §10: packages,
manifests, signatures, the store and verification) in `crates/plugin-pkg` (issue #296), and the
client extension API and its loader with the approval-gate seam ([client-ext-api.md](client-ext-api.md),
issue #293). The loader's use of the store, the offers and the cards aren't built yet.

The client library has no plugin loading today. The server loads `tsmod_*` libraries from
`dlls/` ([plugin-api.md](plugin-api.md) §1.2), but nothing equivalent exists on the client. This
document adds two things:

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
  `<id>-<version>.manifest.toml` and `.manifest.sig`, so the client can show details before it
  downloads anything (§6.2).
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
  again. `cl_plugins_menu` (§10) can undo it.
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

  `xtask` gets `cargo xtask plugin-offer <tspkg>`, which prints the entry for a built package.
* **When offers are sent:** on `ClientPutInServer`, only to port clients (§12). Offers also go
  out on demand, with `sv_plugin_offer <#userid|name|all> <id>` or the plugin API's
  `offer_client_plugin(edict, id)` (the next plugin-api version; tsmod-amxx-rs can expose it as a
  native).
* **Required plugins:** with `sv_plugin_required_kick 1` (default 0), a player who answers
  `denied`, `unsupported` or `failed` to a required offer is kicked. The kick reason names the
  plugin and its homepage. Without it, the plugin API reports the state and plugins decide.
* **What the server learns:** the client's state for each offer, through `tspkg` (§6.5) and the
  plugin API event `client_plugin_state(edict, id, state)`.

### 6.2 The `TSPkg` user message

The port registers `TSPkg` after TS's 64 messages, so TS's registration order is unchanged
(design.md §2).

```
byte    op            1 offer, 2 withdraw (ops 3 and 4: §13.3)
string  id            ≤ 32
string  version       ≤ 16
long    size          package bytes
byte×32 sha256        of the .tspkg
byte    flags         1 required
string  url           ≤ 96; relative to sv_downloadurl, or absolute https://
string  note          ≤ 64
```

That is at most 249 bytes, over GoldSrc's 192-byte user message limit. So `note` is sent only
when `url` is relative and short enough; the client checks the total fits. Open question 1
covers chunking it instead.

### 6.3 Client flow

1. **Receive the offer.** The client checks it first:
   * it ignores offers when `cl_plugins_download 0`;
   * it ignores offers for an id blocked in the store, for this server or always;
   * it answers `have` when the same id, version and package hash is already approved, and loads
     it if it isn't loaded yet (scope permitting).
2. **Fetch the manifest** and its signature (`<url without .tspkg>.manifest.toml` / `.sig`, at
   most 64 KB), on a worker thread. Then check:
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
  redirects limited to 3. HTTPS is required for absolute URLs. A relative URL uses
  `sv_downloadurl` as it is, which is often plain HTTP. That's acceptable because integrity
  comes from the offer's hash and the signature, not the transport. Unsigned packages over plain
  HTTP are always refused. The cost is a TLS stack in `client.dll` (open question 2).
* **Cancel:** disconnecting or pressing Cancel stops the download and deletes the staging file.

### 6.4 Updates

An offer for a newer version of an approved plugin shows the update card (§7.2):
- the old and new versions;
- whether the publisher key is the same;
- the `declares` that changed;
- how many files changed.

It's approved like any offer, so every change prompts. An offer for an older version than the
approved one is shown as a downgrade, with a warning, and "Allow" isn't the default.

### 6.5 Client replies: `tspkg`

```
tspkg <id> <version> <state> [reason]
state: have | offered | downloading | installed | loaded | denied | blocked | unsupported | failed
       | suspended | restart        (§13.3)
tspkg list <id>@<version> ...       every loaded plugin, sent once after the first TSPkg (§13.2)
```

* `denied` covers both "Not now" and "Never". The server isn't told which.
* The server rate-limits `tspkg` (16 per 10 seconds) and ignores ids it didn't offer.

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

### 7.2 Server offer and update cards

These are drawn in game as a port VGUI panel (`crates/client/src/menus/`, next to the MOTD and
team menus).

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
  `password` is given (§13.5). The launcher's relaunch uses it.

None of these can approve. A server that stuffs `cl_plugins 0` or `cl_plugins_menu` can, at
worst, disable plugins for the next start or open the panel. Both are harmless and visible. A
stuffed `tsrs_reconnect` does no more than a stuffed `connect`.

## 9. Loading

* **Approved at start:** loaded at `Initialize` through the client extension API, as specified
  in the webview spec §4.1.
* **Approved later** (a server offer or update): loaded at once, late. The port calls the
  extension's `init`, then `vid_init` if a map is loaded. Extensions must accept that order.
  `client_ext_api` must match exactly; on a mismatch, the reply is `failed api`.
* **`scope = "server"`:** loaded on connecting to a listed address. After that it stays loaded
  until the game exits, even on other servers, because unloading native code isn't safe. The
  card says so. The extension gets a `server_changed(address)` callback, and the webview plugin
  closes its server surface on it.
* **Update while loaded:** the new version is installed and approved, but loads at the next
  start. The card says "restart to finish".
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
  terminal. The e2e roots use it, so runs don't stop at a prompt.
* **Unit tests (`plugin-pkg`):**
  * manifest and path rules (`..`, absolute paths, drive letters, symlinks, duplicate paths);
  * a zip with an extra or missing file;
  * every `Change` variant (changed, missing, extra, manifest, version, key, platform);
  * the store round trip and the `.bad` fallback;
  * signature, countersignature and key change;
  * downgrade detection.
* **Port unit tests:**
  * the offer state machine and the offer queue and limits;
  * the `TSPkg` encoding within 192 bytes;
  * `tspkg` parsing and rate limits on the server.
  * the version policies (`any`, `same_minor`, `exact`, requirements) against `parse_version`;
  * `match` rules, `[[block]]` and `unlisted` decisions;
  * the grace period;
  * the refusal text fitting the reject buffer.
* **e2e (`cargo xtask e2e`, FWGS root with a dedicated server):**
  1. Approve a stub plugin with `plugin-approve`, start, and check it loads. Change one byte, add
     an extra file, delete a file; each must stop it loading, and the log must name the change.
     The prompt is answered by a test-only SDL stub that records what it was shown.
  2. Serve a signed stub package from the root's FastDL and offer it. The test answers the card
     through a test-only hook, which is compiled only into `--features e2e` builds and is never a
     console command. Check the reply sequence `offered`, `downloading`, `loaded`, the installed
     files and the store hashes.
  3. Offer a package whose size or hash doesn't match the offer, a manifest whose signature
     fails, and an unsigned package. Each must answer `failed` or never reach the card, and leave
     nothing in `cl_plugins/`.
  4. An original TS 3.0 client on the same server must not receive `TSPkg` and must play
     normally.
  5. **Version matching:**
     * With `sv_client_version exact`, a client built with another version must be refused at
       connect, and the reason must name both versions. With `sv_client_require_port 1`, the
       original TS client must be refused too.
     * With a `[[block]]` on the stub plugin's version, `suspend` must stop its callbacks (it
       logs every call) and `kick` must drop the client with the reason.
     * A `match = "exact"` offer for a newer stub version must answer `restart`.
     * Restart and reconnect, with a fake launcher that records requests:
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

## 12. Wire compatibility

* `TSPkg` (and the webview spec's `TSExt`) are new user messages. TS 3.0's client doesn't hook
  them, and a GoldSrc client can fail on a user message it hasn't hooked ("UserMsg: Not Present
  on Client"). So the server sends them only to clients known to be the port: those whose
  userinfo carries `_tsrs` (`shared::protocol::port::USERINFO_KEY`), which the server already
  reads in `ClientConnect`. The client takes `TSPkg` support from the port server's version
  (`SrvSett` keys 5–7), so neither side needs a new discovery channel.

  Original clients never get either message, and design.md §2's contract holds.
* The registration order of TS's 64 messages doesn't change. New messages are appended after
  them.
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
  `TSPKG_SINCE` in `shared::protocol::port`): the server lets them in and sends op 5
  `port_version` (§13.3). The client then shows the update card, with the launcher's help where
  it can (§13.5). The server kicks with the refusal text if the client hasn't left within
  `sv_plugin_check_time`.
  This path exists because `client.dll` never sees a `ClientConnect` refusal: the engine shows
  the reason, and the client can't act on it.
* **Older port clients, and TS 3.0's client** (with `sv_client_require_port 1`): refused at
  `ClientConnect` with the text below. They can't receive `TSPkg` (§12).

  Until `TSPkg` ships (#298), `TSPKG_SINCE` doesn't exist and every mismatching port client takes
  this path (issue #300 implements only it, `ts_server::rules::client_version`).

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
* **The plugin API** gets `client_port_version(edict)`, so AMX Mod X plugins can apply a policy
  of their own. A new `PluginApi` entry is a version break, so it waits for the next one (#304).
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
match = "compatible"      # exact | compatible | any | a semver requirement (">=0.2.0, <0.4")
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
  before `1.0.0`, as Cargo's caret rule does. `exact` means the offered version only.
* **What the client reports:** after the first `TSPkg` from the server, the client sends
  `tspkg list` naming every loaded plugin and version, chunked like other commands under 128
  bytes. A client that sends nothing within `sv_plugin_check_time` (default 15 s) is treated as
  having no plugins.
* **Hand-installed libraries** report as `manual:<name>@0.0.0`. Only `unlisted` and an explicit
  `[[block]] id = "manual:*"` apply to them.

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
op 3 require   id, version requirement (≤ 48), flags (1 required)
op 4 block     id, version requirement, byte action (1 suspend, 2 kick), string reason (≤ 64)
op 5 port_version   version requirement (≤ 48), string url (≤ 96); id is "tsrs"
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
* **The plugin API** gets the event `client_plugin_list(edict, plugins)`, and
  `client_plugin_version(edict, id)`.
* **Logs:** the server logs each refusal, suspension and kick with the player, the plugin, the
  version and the rule that matched.

### 13.4 Client cvars

| cvar | default | meaning |
|---|---|---|
| `cl_version_strict` | 0 | 1 disconnects from servers whose port version differs from the client's (`FCVAR_ARCHIVE`; §13.1) |

`cl_plugins_list` shows, for the current server, which loaded plugins are suspended and why.

### 13.5 Restart, reconnect and updates

When the game must restart to get onto a server, because of a plugin update (§13.3) or a port
version the server needs (op 5), the client uses the first of these that's available:

1. **The launcher is running:** the client tells it directly, over IPC.
2. **The launcher is installed but not running:** the client starts it with relaunch arguments.
3. **No launcher, but the game can relaunch itself:** it does.
4. **None of these:** the client offers to quit, and tells the player how to come back.

Updating the port itself always needs the launcher (case 1 or 2), and the launcher always asks
first. Without a launcher, the card says where to get the version and offers to quit.

#### Finding the launcher

* **Launched by the launcher:** tsmod-launcher sets these environment variables on the game
  process, and the client reads them at `Initialize`:
  * `TSMOD_LAUNCHER`: the launcher's executable;
  * `TSMOD_LAUNCHER_INSTANCE`: its installation id (as in `ts-launcher list`);
  * `TSMOD_LAUNCHER_IPC`: the IPC endpoint, while the launcher is running.
* **Started some other way** (from Steam directly, or a desktop shortcut): the launcher writes
  `ts/launcher.toml` into every installation it manages, holding the executable path, the
  installation id and the launcher version. It's rewritten at every launch and install.
* **Checks:** the client accepts a launcher only if the executable exists and the file names
  this game directory as its installation's root. A stale or foreign file is ignored, as if
  there were no launcher.

#### 1. Launcher running: IPC

* **The endpoint:**
  * Linux: a Unix socket, `$XDG_RUNTIME_DIR/ts-launcher-<uid>.sock` (falling back to
    `/tmp/ts-launcher-<uid>/sock`, mode 0700);
  * Windows: the named pipe `\\.\pipe\ts-launcher-<user SID>`, accessible only to the current
    user.
* **The request:** one JSON line per request.

  ```json
  {"op": "relaunch", "instance": "ab12", "pid": 41733,
   "connect": "203.0.113.5:27015", "password_required": true,
   "reason": "port" | "plugin", "port_version": ">=3.0.8, <3.1", "plugin": "webview@0.2.0",
   "server_name": "Bob's TS"}
  ```

* **The handshake:**
  1. The launcher answers `{"ok": true}`, and the client quits the game (`quit`).
  2. The launcher waits for that pid to exit, up to 30 s. It has the `Child` handle when it
     started the game itself.
  3. If the request is for a port update, it asks the player (below).
  4. It relaunches the installation with `+connect <address>`, or with
     `+tsrs_reconnect <address> password` when `password_required` is set (see "Passwords"
     below). This is the launcher's existing `launch(install, opts, extra)`.
* **No answer within 2 s,** or an error: fall back to case 2.

#### 2. Launcher installed, not running: relaunch arguments

The client starts the launcher detached, then quits:

```
ts-launcher relaunch --instance ID --wait-pid PID --connect ADDR [--password-required]
                     [--port-version REQ] [--plugin ID@VERSION] [--server-name NAME]
```

* `relaunch` is a new launcher subcommand. It opens the launcher's window on a "Reconnecting to
  <server>" view that waits for the game to exit, then runs the same steps as the IPC request.
  When the launcher already has an instance open, the new process hands the request to it over
  IPC and exits.

#### Passwords

The password is asked for again after the relaunch; it's never passed on.

* **The client never reads the `password` cvar to send it anywhere.** The IPC request, the
  `relaunch` arguments, the environment and `.reconnect` carry only `password_required`: whether
  the `password` cvar was set when the restart was needed.
* **After the relaunch:** the game starts with `+tsrs_reconnect <address> password` instead of
  `+connect`. `tsrs_reconnect` is a port-only client command. With `password`, it shows a card
  with the server's name and address and a masked password field ("Connect" / "Cancel"), sets
  the `password` cvar from it and connects. Without it, it's the same as `connect`. It can do
  nothing a server's `connect` stufftext can't already do.
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

* **FWGS:** the client re-executes the engine binary with its original arguments plus
  `+connect`. It reads them from `/proc/self/cmdline` on Linux and `GetCommandLineW` on Windows,
  then quits.
* **Steam HL25:** the client opens `steam://run/70//-game ts +connect <address>/`, through
  `xdg-open` or `ShellExecuteW`, then quits. Steam starts the game again once it has exited.
* Each is enabled per engine and platform only once §11's relaunch tests pass there, and
  those tests run in `cargo xtask all` (FWGS) and `cargo xtask steam` (HL25), so a regression
  shows up. Until then, and on a platform where a test fails, the client uses case 4. The
  things to prove are that Steam starts the game again while the old process is still exiting,
  and that the FWGS re-exec rebuilds the right command line under Wine, where the original
  arguments are the Windows command line.

#### 4. Nothing else: quit and inform

* **The card says:** "Restart the game and reconnect to <server name> (<address>)", and for a
  port update also "This server needs version <requirement>; get it from <url> or the TS
  launcher".
* **The address:** it's copied to the clipboard (`SDL_SetClipboardText`).
* **The buttons:** "Quit now" and "Stay" (stay disconnected, or on the server until it kicks).
* **Next start:** the client writes `cl_plugins_data/.reconnect` with the address and a time.
  At the next start, within 10 minutes, the main menu shows "Reconnect to <server>?", a click
  card under §7.3's rules. Nothing connects without that click.

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

1. `TSPkg` can exceed 192 bytes with a long URL and note. Should it chunk, as the webview `TSExt`
   framing does, or move the note into the manifest?
2. **The TLS stack:** `ureq` and `rustls` add about 1.5–2 MB to each `client.dll`. The
   alternative is the engine's own download (`precache_generic` plus `sv_downloadurl`), which
   accepts `.tspkg` names. But it runs at connect time, blocks joining for large packages, and
   can't show a card before downloading.
3. Does `SDL_ShowMessageBox` show correctly over a fullscreen window at `Initialize`, on both
   engines and on Wine? If not, the start-up prompt waits for the first frame and uses the VGUI
   card, and plugins awaiting approval load late (§9).
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
   and the plugin API additions.
4. Client: the offer flow, downloads, the VGUI cards and their safeguards (§7.3), late loading,
   e2e.
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
     resume, restart and reconnect, and the plugin API events.
