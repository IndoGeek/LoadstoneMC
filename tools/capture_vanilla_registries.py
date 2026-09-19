#!/usr/bin/env python3
"""Regenerate LoadstoneMC's synchronized registry + network tag data.

The Configuration state needs the exact set of registries and tags the client
expects for a given protocol version. Rather than hand-maintain that (and risk
drifting from vanilla), this script runs the real vanilla server for the target
version, connects to it with a small client, and records:

  * every Registry Data packet: registry id + ordered entry names
  * the Update Tags packet: registry -> tag -> entry ids

The client negotiates `minecraft:core`, so the server omits entry NBT and the
capture stays small: only names and ordering matter, because LoadstoneMC tells
the client to resolve the NBT from its own copy of the data pack.

Usage:
    python3 tools/capture_vanilla_registries.py --version 1.21.11
    python3 tools/capture_vanilla_registries.py --server-jar /path/to/server.jar

Requires Java 21+ and network access (only to fetch the server jar when
--version is used).
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request
import zlib

MANIFEST_URL = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
PORT = 25599

# Output file layout, relative to the repository root.
DATA_DIR = os.path.join("crates", "loadstone-registry", "data")


# --------------------------------------------------------------------------
# Protocol helpers (client side)
# --------------------------------------------------------------------------


def write_varint(v: int) -> bytes:
    out = bytearray()
    if v < 0:
        v += 1 << 32
    while True:
        b = v & 0x7F
        v >>= 7
        if v:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def read_varint(data: bytes, pos: int):
    value = 0
    shift = 0
    while True:
        b = data[pos]
        pos += 1
        value |= (b & 0x7F) << shift
        if not b & 0x80:
            return value, pos
        shift += 7


def write_string(s: str) -> bytes:
    b = s.encode()
    return write_varint(len(b)) + b


def read_string(data: bytes, pos: int):
    n, pos = read_varint(data, pos)
    return data[pos : pos + n].decode(), pos + n


class Conn:
    def __init__(self, sock: socket.socket):
        self.sock = sock
        self.buf = b""
        self.compression = False

    def recv_exact(self, n: int) -> bytes:
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def read_packet(self):
        raw = b""
        while True:
            b = self.recv_exact(1)[0]
            raw += bytes([b])
            if not b & 0x80:
                break
        length, _ = read_varint(raw, 0)
        body = self.recv_exact(length)
        if self.compression:
            data_len, pos = read_varint(body, 0)
            body = body[pos:] if data_len == 0 else zlib.decompress(body[pos:])
        pid, pos = read_varint(body, 0)
        return pid, body[pos:]

    def send_packet(self, pid: int, payload: bytes = b""):
        data = write_varint(pid) + payload
        if self.compression:
            data = write_varint(0) + data
        self.sock.sendall(write_varint(len(data)) + data)


# Configuration packet ids (protocol 774).
CB_KNOWN_PACKS = 0x0E
CB_REGISTRY_DATA = 0x07
CB_TAGS = 0x0D
CB_FINISH = 0x03
CB_KEEP_ALIVE = 0x04
CB_PING = 0x05
CB_DISCONNECT = 0x02
SB_SETTINGS = 0x00
SB_KEEP_ALIVE = 0x04
SB_PONG = 0x05
SB_KNOWN_PACKS = 0x07
SB_FINISH = 0x03


def capture(protocol: int, host: str, port: int):
    sock = socket.create_connection((host, port), timeout=15)
    c = Conn(sock)

    c.send_packet(
        0x00,
        write_varint(protocol) + write_string("localhost") + struct.pack(">H", port) + write_varint(2),
    )
    c.send_packet(0x00, write_string("Capturer") + b"\x00" * 16)

    while True:
        pid, body = c.read_packet()
        if pid == 0x03:  # Set Compression
            c.compression = True
        elif pid == 0x02:  # Login Success
            break
        elif pid == 0x00:  # Disconnect
            reason, _ = read_string(body, 0)
            raise SystemExit(f"disconnected during login: {reason}")

    c.send_packet(0x03)  # Login Acknowledged

    settings = (
        write_string("en_us")
        + struct.pack(">b", 4)
        + write_varint(0)
        + b"\x01"
        + b"\x7f"
        + write_varint(1)
        + b"\x00\x01"
        + write_varint(0)
    )
    c.send_packet(SB_SETTINGS, settings)

    registries = {}
    tags = {}
    order = []
    while True:
        pid, body = c.read_packet()
        if pid == CB_KNOWN_PACKS:
            count, pos = read_varint(body, 0)
            packs = []
            for _ in range(count):
                ns, pos = read_string(body, pos)
                pk, pos = read_string(body, pos)
                ver, pos = read_string(body, pos)
                packs.append((ns, pk, ver))
            out = write_varint(len(packs))
            for ns, pk, ver in packs:
                out += write_string(ns) + write_string(pk) + write_string(ver)
            c.send_packet(SB_KNOWN_PACKS, out)
        elif pid == CB_REGISTRY_DATA:
            reg, pos = read_string(body, 0)
            count, pos = read_varint(body, pos)
            entries = []
            for _ in range(count):
                name, pos = read_string(body, pos)
                has_data = body[pos]
                pos += 1
                if has_data:
                    raise SystemExit("unexpected registry NBT; known-pack negotiation failed")
                entries.append(name)
            registries[reg] = entries
            order.append(reg)
        elif pid == CB_TAGS:
            count, pos = read_varint(body, 0)
            for _ in range(count):
                tag_type, pos = read_string(body, pos)
                ntags, pos = read_varint(body, pos)
                out_tags = []
                for _ in range(ntags):
                    tname, pos = read_string(body, pos)
                    n, pos = read_varint(body, pos)
                    ids = []
                    for _ in range(n):
                        v, pos = read_varint(body, pos)
                        ids.append(v)
                    out_tags.append({"name": tname, "entries": ids})
                tags[tag_type] = out_tags
        elif pid == CB_KEEP_ALIVE:
            c.send_packet(SB_KEEP_ALIVE, body)
        elif pid == CB_PING:
            c.send_packet(SB_PONG, body)
        elif pid == CB_FINISH:
            c.send_packet(SB_FINISH)
            break
        elif pid == CB_DISCONNECT:
            reason, _ = read_string(body, 0)
            raise SystemExit(f"disconnected during configuration: {reason}")

    sock.close()
    return registries, tags, order


# --------------------------------------------------------------------------
# Server lifecycle
# --------------------------------------------------------------------------


def download_server(version: str, dest: str) -> str:
    with urllib.request.urlopen(MANIFEST_URL, timeout=60) as resp:
        manifest = json.load(resp)
    entry = next((v for v in manifest["versions"] if v["id"] == version), None)
    if entry is None:
        raise SystemExit(f"unknown version {version}")
    with urllib.request.urlopen(entry["url"], timeout=60) as resp:
        meta = json.load(resp)
    url = meta["downloads"]["server"]["url"]
    print(f"downloading server jar for {version} ...", file=sys.stderr)
    urllib.request.urlretrieve(url, dest)
    return dest


def start_server(server_jar: str, workdir: str) -> subprocess.Popen:
    with open(os.path.join(workdir, "eula.txt"), "w") as fh:
        fh.write("eula=true\n")
    with open(os.path.join(workdir, "server.properties"), "w") as fh:
        fh.write(
            "online-mode=false\n"
            f"server-port={PORT}\n"
            "level-type=minecraft:flat\n"
            "spawn-protection=0\n"
            "max-players=1\n"
            "view-distance=4\n"
            "level-name=captureworld\n"
        )
    log = open(os.path.join(workdir, "server.log"), "w")
    proc = subprocess.Popen(
        ["java", "-Xmx1G", "-jar", os.path.abspath(server_jar), "nogui"],
        cwd=workdir,
        stdout=log,
        stderr=subprocess.STDOUT,
    )
    deadline = time.time() + 180
    log_path = os.path.join(workdir, "server.log")
    while time.time() < deadline:
        if os.path.exists(log_path) and "Done (" in open(log_path).read():
            return proc
        if proc.poll() is not None:
            raise SystemExit("server exited before it finished starting")
        time.sleep(1)
    proc.terminate()
    raise SystemExit("timed out waiting for the server to start")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", default="1.21.11", help="Minecraft version to capture")
    parser.add_argument("--protocol", type=int, default=774, help="Protocol version for the handshake")
    parser.add_argument("--server-jar", help="Existing server.jar (skips the download)")
    parser.add_argument("--data-dir", default=DATA_DIR, help="Where to write the JSON files")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory() as workdir:
        server_jar = args.server_jar or download_server(
            args.version, os.path.join(workdir, "server.jar")
        )
        proc = start_server(server_jar, workdir)
        try:
            registries, tags, order = capture(args.protocol, "127.0.0.1", PORT)
        finally:
            proc.terminate()
            proc.wait(timeout=30)

    os.makedirs(args.data_dir, exist_ok=True)
    synced = {
        "minecraft_version": args.version,
        "protocol": args.protocol,
        "known_pack": {"namespace": "minecraft", "id": "core", "version": args.version},
        "registries": [{"id": rid, "entries": registries[rid]} for rid in order],
    }
    # Registry *order* is meaningful (it assigns entry ids) and is kept as sent,
    # but tag order is not, and the vanilla server emits it in hash order — sort
    # it so regenerating produces identical files.
    network_tags = {
        "minecraft_version": args.version,
        "registries": [
            {
                "registry": rid,
                "tags": sorted(tags[rid], key=lambda tag: tag["name"]),
            }
            for rid in sorted(tags)
        ],
    }
    with open(os.path.join(args.data_dir, "synced_registries.json"), "w") as fh:
        json.dump(synced, fh, indent=1)
        fh.write("\n")
    with open(os.path.join(args.data_dir, "network_tags.json"), "w") as fh:
        json.dump(network_tags, fh, indent=1)
        fh.write("\n")

    print(
        f"captured {len(order)} registries and "
        f"{sum(len(v) for v in tags.values())} tags into {args.data_dir}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
