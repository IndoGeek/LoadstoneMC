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
| Configuration state | not implemented |
| Play state (world, chunks, entities, chat) | not implemented |

A vanilla client can ping the server and complete login. Because the
Configuration state does not exist yet, the server closes the connection right
after the client acknowledges login, so the player stops at "Joining world".

## Build and run

Requires Rust 1.80 or newer.

```bash
cargo build --release
cargo run -p loadstone-server --release -- --bind 0.0.0.0:25565 --motd "LoadstoneMC"
```

```bash
# Online mode: encrypts the connection and verifies accounts with Mojang.
cargo run -p loadstone-server --release -- --online-mode
```

| Flag | Default | Meaning |
|---|---|---|
| `--bind` | `0.0.0.0:25565` | Address to listen on |
| `--motd` | `A LoadstoneMC server` | Server list description |
| `--max-players` | `20` | Players shown in the server list |
| `--online-mode` | off | Require encryption + Mojang session verification |
| `--sessionserver-url` | Mojang's `hasJoined` | Base URL for session verification |

Logging is controlled by `RUST_LOG` (for example `RUST_LOG=loadstone=debug`).

## Layout

| Crate | Contents |
|---|---|
| `loadstone-protocol` | Packet definitions, VarInt, packet reader/writer, compression |
| `loadstone-net` | Connection lifecycle, framing, AES-128/CFB8, login flow, session auth |
| `loadstone-world` | Placeholder — chunk storage and world state |
| `loadstone-registry` | Placeholder — registry data served during Configuration |
| `loadstone-server` | The `loadstone` binary: CLI, listener, per-connection tasks |

`tools/live_login_check.py` is an independent protocol client used to check a
running server (see below). `data/` holds generated data that is not part of the
build.

## Verifying

```bash
cargo test                                   # unit + integration tests
cargo clippy --workspace --all-targets       # must stay silent
cargo fmt --all --check
```

`crates/loadstone-net/tests/login_flow.rs` starts a real listener and drives the
whole login handshake, including online mode against a local mock session
server, so no Mojang account or network access is needed.

`tools/live_login_check.py` does the same over a socket against a real running
binary, with a client written independently of the server (Python `cryptography`):

```bash
python3 tools/live_login_check.py                 # spawns ./target/release/loadstone, offline mode
python3 tools/live_login_check.py --mode online   # spawns it with --online-mode + mock session server
python3 tools/live_login_check.py --no-spawn --port 25565
```

It covers the server list ping, an offline login, an online login (key exchange
→ AES/CFB8 → `hasJoined` → compression → Login Success → acknowledgement), and
both online-mode refusals: an unverified account, and a key exchange that does
not echo the verify token. Exit status is non-zero if anything fails, so it can
gate a release.

## Protocol notes

- The version constants live in `crates/loadstone-protocol/src/lib.rs`.
- Packet layouts for 1.21.11 were checked against the PrismarineJS
  `minecraft-data` protocol dump for that version.
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
- Compression is enabled at a 256-byte threshold, matching the vanilla default.

## Roadmap

1. **Configuration state** — registry data, feature flags, known packs, tags,
   finish configuration. This is what a client needs before it can enter Play.
2. **Play state** — join game, chunk data, player position, keep-alive, chat
   (including verifying the Mojang-signed session key chain).
3. **World** — chunk format on the wire and on disk, generation, persistence.
4. **Registry data generation** from vanilla reports instead of hand-written
   entries.

## License

MIT, as declared in `Cargo.toml`. The `LICENSE` file itself is not committed yet.
