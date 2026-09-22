# LoadstoneMC

A Minecraft: Java Edition server written from scratch in Rust, aiming for a
small, fast, low-overhead core in the spirit of Pumpkin and Steel.

**Target version: Minecraft 1.21.11 — protocol 774.**

## Status

| Area | State |
|---|---|
| Server list ping (version, MOTD, player counts, ping/pong) | works |
| Login, offline mode | works |
| Login, online mode (RSA key exchange, AES-128/CFB8, Mojang `hasJoined`, compression) | works |
| Configuration state (known packs, feature flags, registry data, tags, finish) | works |
| Play state (login, world spawn, position, keep-alive, chat echo, block dig/place) | works |
| Multiple players in one shared world (tab list, spawn/despawn, movement sync, shared block edits) | works |
| Chunk streaming as players cross chunk borders (batched loads, unloads, view centre) | works |
| Terrain generation (deterministic value-noise hills, bedrock/stone/dirt/grass layers) | works |
| World persistence (vanilla Anvil region files, seed sidecar, autosave, save on shutdown) | works |
| Non-player entities (pigs, cows, sheep, chickens, zombies) with gravity, collision, wander/chase AI | works |
| `server.properties` with vanilla 1.21.11's full key set, and the vanilla EULA gate | works (10 keys acted on, the rest preserved) |

A vanilla client can ping the server, complete login, and finish the whole
Configuration handshake: it negotiates `minecraft:core`, receives every
synchronized registry and network tag, acknowledges Finish Configuration, and
then spawns onto generated terrain in the Overworld. Terrain comes from a
seeded, deterministic value-noise generator: surface height varies around y=64
with three octaves of hills, each column is bedrock at the bottom, stone up to a
few blocks below the surface, dirt, then grass, and the chunk is split into the
24 sections protocol 774 expects (empty sections collapse to an air-only
palette). The Play state sends the world spawn and chunk batch (with
ultra-precise heightmaps and full-bright sky light), acknowledges the chunk
batch, teleports the player to spawn, enters the tab list, and runs keep-alive,
ping/pong, teleport confirmations and chat echo (including the Mojang-signed
session key chain on online mode) until the client disconnects. Walking into a
neighbouring chunk moves the client's view centre (`set_chunk_cache_center`),
streams the columns that entered view as a chunk batch and sends `unload_chunk`
for the ones that left it. The world is mutable: digging a block or placing
another is validated against a shared in-memory model (bedrock is unbreakable,
out-of-bounds edits are ignored) and the resulting block change is broadcast to
every player. All connections share that world and a player registry: joining
players are shown the players already present (tab-list entry, `spawn_entity`
and skin-layer metadata, wrapped in a bundle), and movement, chat and block
edits are pushed to the other players' connections. Leaving players are
announced with `player_remove` + `remove_entities`. Generated-and-edited chunks
are persisted in vanilla Anvil format: dirty chunks are written to
`<world>/region/r.X.Z.mca` periodically and on shutdown, and the seed is kept in
a `<world>/loadstone.seed` sidecar so a restart regenerates the same terrain and
overlays the saved edits. A small deterministic mob population (pigs, cows,
sheep, chickens and a zombie) is spawned around the world spawn and simulated at
20 Hz: each mob falls under gravity, collides with blocks, wanders around its
home or, if hostile, walks towards the nearest player within 16 blocks. Mobs
are streamed to each player as `spawn_entity` when their chunk enters that
player's view and kept in step with `sync_entity_position`, then removed with
`remove_entities` when they leave it; the ticker idles while nobody is online.

## Build and run

Requires Rust 1.80 or newer.

```bash
cargo build --release
cargo run -p loadstone-server --release -- --accept-eula
```

From the project root, the `loadstone` launcher builds the release binary if
needed and forwards every flag to it, so `./loadstone` is enough:

```bash
./loadstone --accept-eula                     # first run: accept the EULA and boot
./loadstone                                   # every run after that

# Online mode: encrypts the connection and verifies accounts with Mojang.
./loadstone --online-mode

# Or: LOADSTONE_BIN=/path/to/loadstone ./loadstone          # prebuilt binary
#     LOADSTONE_NO_BUILD=1 ./loadstone                     # do not invoke cargo
#     CARGO=/path/to/cargo ./loadstone                     # pick a specific cargo
```

The server runs in the directory you start it from (or `--dir`), where it keeps
`server.properties`, `eula.txt` and the world. Like vanilla, it refuses to start
until the Minecraft EULA has been accepted, which `--accept-eula` records in
`eula.txt` once so later starts need no flag. Every flag is optional apart from
that first acceptance: the settings come from `server.properties`.

The launcher prefers the rustup toolchain (`~/.cargo/bin/cargo`) over a possibly
older distro `cargo`, and falls back to an existing `target/release/loadstone`
binary if the only cargo it can find is too old (this project needs 1.80+).

### Prebuilt binaries

Release archives are published on GitHub for Linux (x86-64, arm64), macOS
(x86-64, arm64) and Windows (x86-64). The Linux builds are statically linked
against musl, so the binary has no glibc requirement and runs on any
distribution. Each archive contains the self-contained `loadstone` server
binary (no Java needed), the README, the LICENSE and the Pterodactyl egg:

```bash
tar -xzf loadstone-x86_64-unknown-linux-musl.tar.gz
cd loadstone-x86_64-unknown-linux-musl
./loadstone --bind 0.0.0.0:25565
```

`tools/package.sh [target-triple]` builds the same archive locally and writes it
to `dist/`; on Linux it defaults to the static musl target when that target is
installed (`rustup target add x86_64-unknown-linux-musl`).

### Pterodactyl

`pterodactyl/egg-loadstone.json` is a ready-to-import egg. The installer detects
the machine architecture, downloads the matching static release binary, makes it
executable and writes `eula.txt` (installing the egg is the operator accepting
the EULA, as with the official Minecraft egg); the runtime uses the Debian yolks
image and needs no Java. The egg exposes the MOTD, max players, online mode,
save interval and extra arguments as variables, binds the server to the
allocated `SERVER_PORT`, and stops it gracefully by sending `stop` on stdin.
Import it from **Nests → Import Egg**.

Because the Pterodactyl entrypoint word-splits the startup command, the MOTD is
not passed as an argument: the server reads the `MOTD` environment variable
instead, so a description containing spaces works fine.

Every flag except `--dir` and `--accept-eula` overrides the matching
`server.properties` key only when it is actually given; on its own the file
decides.

| Flag | Default | Meaning |
|---|---|---|
| `--dir` | `.` | Directory holding `server.properties`, `eula.txt` and the world |
| `--accept-eula` | off | Accept the Minecraft EULA, writing `eula.txt` |
| `--bind` | `server-ip`:`server-port` | Address to listen on |
| `--motd` | `MOTD` env, else `motd` | Server list description |
| `--max-players` | `max-players` | Players shown in the server list |
| `--online-mode` | `online-mode` | Require encryption + Mojang session verification |
| `--sessionserver-url` | Mojang's `hasJoined` | Base URL for session verification |
| `--world` | `level-name`, under `--dir` | World directory holding `region/` and the seed sidecar |
| `--seed` | saved seed, else `0` | Terrain seed for a new world |
| `--save-interval` | `30` | Seconds between autosaves of dirty chunks (`0` disables) |
| `--colour` | `auto` | Colour the console: `auto` (only on a terminal), `always`, `never` |

### Configuration

A first run writes a `server.properties` containing **exactly the 67 keys a real
1.21.11 server writes for itself, in the same order** (alphabetical, as Java's
`Properties.store` emits them), so the file can be diffed against a vanilla one
line for line. `crates/loadstone-server/tests/data/vanilla-server.properties`
is the captured reference and a test compares the built-in table against it in
keys, order and values, which is how `enable-command-block` (dropped in 1.21.11)
and the whole `management-server-*` family were found to be wrong here before.
Keys vanilla has since dropped are not added, and a file that still carries them
keeps them.

Ten keys take effect today: `server-ip`, `server-port`, `motd`, `max-players`,
`online-mode`, `level-name`, `enable-status`, `network-compression-threshold`,
`gamemode` and `hardcore`. The other 57 are read, written back verbatim and
counted in the startup log:

```
INFO server.properties loaded path=./server.properties honoured=10 not_acted_on_yet=57
```

so a setting is never silently dropped before the feature behind it exists —
`rcon`, `white-list` and `resource-pack-*` need subsystems (RCON, player data, a
transfer path) that are not here yet. `view-distance` and
`simulation-distance` are deliberately in that second group: the chunk streamer
keeps its own small radius so a joining client finishes "downloading terrain"
quickly, and having the file say 10 while the login packet says 1 would be worse
than either value alone. Two other deliberate deviations: `level-type` is
written unescaped (`minecraft:normal`, not `minecraft\:normal`) though the
escaped form is read correctly, and `management-server-secret` is left empty
rather than generated, since nothing here speaks that protocol.

The server saves dirty chunks on a timer and force-saves everything on shutdown:
`Ctrl-C`/SIGINT, `SIGTERM`, or `stop`/`exit`/`quit` typed on stdin (which is how
Pterodactyl stops it). World files are vanilla-compatible Anvil regions
(`DataVersion 4671`), so an existing vanilla 1.21.11 world can be dropped in and
edited.

### Console and logging

Every line is printed in the layout a vanilla server's console uses, coloured on
standard output and plain in `logs/latest.log`:

```
[05:16:36] [Server thread/INFO]: LoadstoneMC 0.1.0 — Minecraft 1.21.11, protocol 774
[05:16:36] [Server thread/INFO]: Starting minecraft server version 1.21.11
[05:16:36] [Server thread/INFO]: Loading properties
[05:16:36] [Server thread/INFO]: server.properties loaded path=./server.properties honoured=10 not_acted_on_yet=57
[05:16:36] [Server thread/INFO]: Default game type: SURVIVAL
[05:16:36] [Server thread/INFO]: Generating keypair
[05:16:36] [Server thread/INFO]: Starting Minecraft server on *:25566
[05:16:36] [Server thread/WARN]: **** SERVER IS RUNNING IN OFFLINE/INSECURE MODE!
[05:16:36] [Server thread/INFO]: Preparing level "world"
[05:16:36] [Server thread/INFO]: Loading 12 persistent chunks...
[05:16:36] [Server thread/INFO]: world ready dir=./world seed=0 loaded_chunks=12
[05:16:36] [Server thread/INFO]: spawned mobs mobs=7
[05:16:36] [Server thread/INFO]: Time elapsed: 2 ms
[05:16:36] [Server thread/INFO]: Done (0.043s)! For help, type "help"
```

The start-up lines are **vanilla 1.21.11's own wording, in its order**, taken from
a captured run of the real server rather than from documentation. Four are ours
and have no vanilla equivalent (the banner, the properties summary, `world ready`
and `spawned mobs`), and two vanilla lines are deliberately absent: this server
selects no global world spawn and prepares no spawn area at start-up (chunks
around spawn are generated on first join), so printing vanilla's
`Selecting global world spawn...` and `Preparing spawn area: 100%` would be
dressing up work that does not happen. Failures use vanilla's wording too, for
the same reason an operator knows them: the EULA refusal, the offline-mode
warnings, and

```
[Server thread/WARN]: **** FAILED TO BIND TO PORT!
[Server thread/WARN]: The exception was: Address already in use (os error 98)
[Server thread/WARN]: Perhaps a server is already running on that port?
```

The console accepts `stop` (also `exit`, `quit`, `shutdown`) and `help`, which is
what the `Done (...)` line points at; anything else is answered with vanilla's

```
[Server thread/INFO]: Unknown or incomplete command. See below for error
[Server thread/INFO]: <what was typed><--[HERE]
```

That `Done (...)` line is also what the Pterodactyl egg watches for, so the panel
flips to "running" when the world is up rather than when the socket opens.

The level is coloured by severity and the timestamp and any structured context
after the message are dimmed, so the message itself is what stands out. Player
lines (join, leave, chat) additionally colour the text: `crates/loadstone-server/src/logging.rs`
exposes `component` and `style` fields for that. Context such as `honoured=10`
is the structured logging the code already does, kept visible without drowning
the message.

Online mode generates its RSA keypair once at start-up (`Generating keypair`) and
every login reuses it, the way vanilla does, which also keeps a 1024-bit key
generation off the login path.

`logs/latest.log` holds the same lines without escape codes, and the previous one
is renamed to `latest.log.1` at startup rather than compressed, so it stays
greppable. Console colour follows the terminal by default; a panel has no
terminal of its own but renders escape codes, which is why the Pterodactyl egg
passes `--colour always`. Verbosity is set with `RUST_LOG` (for example
`RUST_LOG=loadstone=debug`).

The server stops on Ctrl-C/SIGINT, on `SIGTERM`, or on `stop`/`exit`/`quit` typed
at the console, then saves the world and flushes the log. Console input is read on
its own OS thread, not through the async runtime: a blocking read parked in the
runtime's blocking pool is never woken by an idle terminal, and dropping the
runtime would wait for it forever, so the process would finish its whole shutdown
and then hang instead of exiting.

## Layout

| Crate | Contents |
|---|---|
| `loadstone-protocol` | Packet definitions, VarInt, packet reader/writer, compression |
| `loadstone-net` | Connection lifecycle, framing, AES-128/CFB8, login flow, session auth |
| `loadstone-world` | Terrain generation, the mutable world model, mob entities with gravity/collision/AI, Anvil region persistence, and the 1.21.11 chunk wire format (paletted containers, heightmaps, sky light) |
| `loadstone-registry` | Synchronized registry and network tag data (embedded JSON) |
| `loadstone-server` | The `loadstone` binary: CLI, `server.properties` + EULA handling, listener, per-connection tasks |

`crates/loadstone-registry/data/` holds the captured 1.21.11 registry and tag
data, embedded into the binary at build time with `include_str!`. It is
regenerated by `tools/capture_vanilla_registries.py`, which runs the real vanilla
server for the target version and records exactly which registries and tags it
sends during Configuration.

`tools/live_login_check.py` is an independent protocol client used to check a
running server (see below). `data/` holds scratch generated data that is not
part of the build.

## Verifying

```bash
cargo test                                   # unit + integration tests
cargo clippy --workspace --all-targets       # must stay silent
cargo fmt --all --check
```

`.github/workflows/ci.yml` enforces exactly those three (clippy with
`-D warnings`) and then runs `tools/live_login_check.py` in both modes against
the release binary. `regenerate-registry-data.yml` is a manual job that
re-captures the registry data from a real vanilla server when the target version
changes, so staleness fails a build instead of going unnoticed.

`crates/loadstone-net/tests/login_flow.rs` starts a real listener and drives the
whole login handshake through Configuration and into the Play state, including
online mode against a local mock session server, so no Mojang account or network
access is needed. It asserts the server offers `minecraft:core`, sends all 23
synchronized registries with their NBT omitted, finishes configuration when the
client acknowledges, and then spawns a simulated player: it checks the Play login
packet's dimension/spawn info, the 3x3 chunk batch (ultra-precise heightmaps,
which must not all be identical, and non-empty chunk data), the spawn position,
the teleport onto the generated surface at (8.5, surface+1, 8.5), full
health/hunger, the tab list entry, and the welcome chat line carrying the
player's name. Another test walks one chunk east and checks that the view centre
moves, the three newly visible columns are streamed as a batch, and the three
that fell out of view are unloaded. A third test runs two clients against one
server and checks that each sees the other spawn (`spawn_entity`, type 155), that
movement arrives as an entity sync, that a block edit by one is broadcast to the
other, and that a disconnect produces `player_remove` + `remove_entities`. A
fourth test starts the entity ticker and checks that a joining player is sent all
seven starting mobs with their `minecraft:entity_type` ids and then sees them
move. Three more pin the configuration keys to the packets they change:
gamemode/hardcore/max-players reach the Play login packet, a negative
`network-compression-threshold` means no Set Compression and an uncompressed
handshake, and `enable-status=false` closes a ping to the status port instead of
answering it. `crates/loadstone-server/src/properties.rs` carries its own tests
for the key set, the defaults, escaping and round-trip stability.

`tools/live_login_check.py` does the same over a socket against a real running
binary, with a client written independently of the server (Python `cryptography`):

```bash
python3 tools/live_login_check.py                 # spawns ./target/release/loadstone, offline mode
python3 tools/live_login_check.py --mode online   # spawns it with --online-mode + mock session server
python3 tools/live_login_check.py --no-spawn --port 25565
```

It covers the server list ping, an offline login, chunk streaming as the player
walks across a chunk border, an online login (key exchange → AES/CFB8 →
`hasJoined` → compression → Login Success → acknowledgement → the full
Configuration handshake → the Play state), a two-client shared-world check
(presence, movement, shared block edit, disconnect), a mob check (the seven
starting entities spawn with the right registry type ids and move), and both
online-mode refusals: an unverified account, and a key exchange that does not
echo the verify token. Offline mode then interrupts the server and inspects the Anvil
region it wrote, checking the 8 KiB header, a zlib payload with a `DataVersion`,
and that both generated terrain and a player edit survived to disk. Exit status
is non-zero if anything fails, so it can gate a release.

## Protocol notes

- The version constants live in `crates/loadstone-protocol/src/lib.rs`.
- Packet layouts for 1.21.11 were checked against the PrismarineJS
  `minecraft-data` protocol dump for that version, and the Play-state ids were
  additionally confirmed by capturing live traffic from a real 1.21.11 vanilla
  server.
- Protocol 774 has no dedicated player-spawn packet: other players are
  `spawn_entity` with entity type 155 (`minecraft:player`), sent inside a bundle
  delimiter pair together with their tab-list entry and entity metadata. The
  `minecraft:entity_type` registry is static (not synchronized), so these ids
  come from the vanilla data generator rather than the registries sent at login;
  `minecraft:slime` is 117.
- Chunk batches use `chunk_batch_start` id `0x0C` and `chunk_batch_finished` id
  `0x0B` (the latter carries a VarInt batch size, not a float). Note the finish
  id is lower than the start id, and `unload_chunk` sends chunk Z before chunk X.
- **`chunk_batch_received` answers `chunk_batch_finished`, and never precedes
  it.** The client measures the batch and sends its receipt only once the batch
  is closed, so a server that waits for the receipt before sending the finish
  packet deadlocks against a real client, which sits on "Loading terrain" until
  it times out. Both test clients used to acknowledge after the ninth chunk,
  which is exactly what the server waited for, so neither could see it; they now
  acknowledge on `chunk_batch_finished` and the live check asserts it.
- **Tab-list action bits are ordered, and a set bit can still carry nothing.**
  The order is add-player, initialize-chat, game-mode, listed, latency,
  display-name, list-order, hat (`0x01`…`0x80`), and vanilla sets every bit of a
  mask with the "present but empty" form for the optional fields (a `false` flag
  for the chat session and the display name). A model that treats "bit clear" and
  "bit set, payload absent" as the same state re-encodes such a packet one byte
  short per field.
- **A text component owns only its own bytes.** Components such as the tab-list
  display name, the server-data MOTD and a system chat line are variable-length
  anonymous NBT, and fields can follow them in the same packet. Reading "the rest
  of the packet" as the component swallows whatever follows it; parse the tag and
  take its length instead.
- Encryption is RSA-1024 with PKCS#1 v1.5 for the key exchange, then
  AES-128/CFB8 over the whole stream with the shared secret as both key and IV,
  exactly as Java's `AES/CFB8/NoPadding` does.
- **CFB8 derives a fresh block from the feedback register for every byte.** A
  round trip through one implementation cannot catch a mistake here, so
  `crypto.rs` pins reference vectors taken from OpenSSL and `cryptography`,
  which agree byte for byte. Do not replace those with a round-trip test.
- The verify token is checked by echo: protocol 774's Encryption Response has no
  signature field (profile-key signatures left login in 1.19.3). Mojang-signed
  key chains arrive later, in the Play state as a serverbound
  `chat_session_update`, and that is where signature verification belongs.
- **The echoed verify token is RSA-encrypted, like the shared secret**, so the
  echo is decrypted with the server's private key before it is compared. A
  comparison against the bytes as received can only ever match a client that
  wrongly sends the token in the clear, which is what makes this bug easy to
  write and hard to notice: a test client that mirrors the server will pass.
- **The `serverId` hash is Java's signed `BigInteger` hex.** With vanilla's empty
  server id it is `SHA-1(sharedSecret ++ publicKey)` read as a *signed* big-endian
  integer, so a digest whose first bit is set prints with a leading `-` and a
two's-complement magnitude, and leading zero bytes are dropped. Hex-dumping the
  digest (or joining it with `-` like the pre-1.7 session id) produces a string
  the client never sent, and Mojang answers it with 204, which surfaces as a
  login failure long after the key exchange looked fine. `auth.rs` pins this to
  values produced by Java's `new BigInteger(...).toString(16)` itself.
- Compression is enabled at a 256-byte threshold, matching the vanilla default,
  and `network-compression-threshold` moves or disables it: a negative value
  suppresses the Set Compression packet and leaves the stream uncompressed.
- **Known packs.** The server offers `minecraft:core` and expects it back. Once
  the client confirms it knows the pack, every synchronized registry entry is
  sent with its NBT omitted and the client resolves the data from its own copy of
  the data pack, so only entry names and order travel over the wire. `core` is
  the only pack a vanilla client knows, so a client that does not echo it is
  disconnected with a clear reason rather than sent incomplete data.
- **Region files are vanilla Anvil.** Chunks are written with
  `DataVersion 4671` (matching `version.json` for 1.21.11) as a named-tag root
  with `sections` (palette + LSB-first packed `data` at a minimum of 4 bits),
  `biomes`,   `Heightmaps` and `isLightOn`; each chunk sits in an 8 KiB header
  (3-byte sector offset + one-byte sector count), then a `u32` length, a
  compression byte `2` (zlib) and the compressed payload. The format was checked
  against real `r.0.0.mca` output from a vanilla 1.21.11 server. Since only
  terrain and player edits are tracked, no block entities, ticks or light data
  are written yet.
- **Registry order is protocol.** A registry's entries are sent in the order
  that assigns their numeric ids, so `data/synced_registries.json` preserves the
  order the vanilla server uses. Tag order is not meaningful and is sorted by the
  generator so regeneration is byte-identical.

## Roadmap

1. **Game feel** — richer mobs. Terrain generation, Anvil persistence and a
   first wander/chase mob population are in place; the next steps are combat and
   damage, a broader block set, and light/block-entity data in the saved chunks.
2. **Custom registries** — the current data is the vanilla set with NBT omitted;
   serving custom biomes/dimensions means emitting entry NBT as well.
3. **Player data & chat** — verify the Mojang-signed session key chain for chat
   on online mode, and persist players between sessions.

## License

MIT, as declared in `Cargo.toml` and in the `LICENSE` file.
