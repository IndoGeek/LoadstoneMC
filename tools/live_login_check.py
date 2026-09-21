#!/usr/bin/env python3
"""Live client checks against a LoadstoneMC server.

Speaks the wire protocol itself, the way a vanilla client does, so the server is
verified over a real socket instead of in-process: server list ping, login
(offline and online modes, including the RSA key exchange with AES/CFB8
streaming and a Mojang-style `hasJoined` lookup against a local mock session
server), the full Configuration handshake, and the Play state where the client
spawns onto generated terrain, chat is echoed back, and block edits (dig/place)
are acknowledged with block changes. Also covers the two
online-mode refusals: an unverified account, and a key exchange that does not
echo the verify token.

    python3 tools/live_login_check.py                 # spawns ./target/release/loadstone (offline)
    python3 tools/live_login_check.py --mode online   # spawns it with --online-mode + mock session server
    python3 tools/live_login_check.py --no-spawn      # checks an already running server

Exits non-zero when any check fails, so it can gate a release.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import glob
import os
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import uuid
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

try:
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric import padding
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
except ImportError:  # pragma: no cover - environment guard, not a test
    print("FAIL: the 'cryptography' package is required for live checks (pip install cryptography)")
    sys.exit(2)

PROTOCOL_VERSION = 774
MINECRAFT_VERSION = "1.21.11"

# Packet ids used by the checks (status, login, configuration and play states).
PKT_HANDSHAKE = 0x00
PKT_STATUS_REQUEST = 0x00
PKT_STATUS_RESPONSE = 0x00
PKT_PING = 0x01
PKT_PONG = 0x01
PKT_LOGIN_START = 0x00
PKT_LOGIN_DISCONNECT = 0x00
PKT_ENCRYPTION_REQUEST = 0x01
PKT_ENCRYPTION_RESPONSE = 0x01
PKT_LOGIN_SUCCESS = 0x02
PKT_SET_COMPRESSION = 0x03
PKT_LOGIN_ACKNOWLEDGED = 0x03

# Configuration state.
PKT_CONFIG_CUSTOM_PAYLOAD = 0x01
PKT_CONFIG_FEATURE_FLAGS = 0x0C
PKT_CONFIG_KNOWN_PACKS = 0x0E
PKT_CONFIG_REGISTRY_DATA = 0x07
PKT_CONFIG_UPDATE_TAGS = 0x0D
PKT_CONFIG_FINISH = 0x03
PKT_CONFIG_CLIENT_INFO = 0x00
PKT_CONFIG_SELECT_PACKS = 0x07
PKT_CONFIG_ACK = 0x03

# Play state, clientbound.
PKT_PLAY_BUNDLE_DELIMITER = 0x00
PKT_PLAY_SPAWN_ENTITY = 0x01
PKT_PLAY_BLOCK_CHANGE = 0x08
PKT_PLAY_CHUNK_BATCH_FINISHED = 0x0B
PKT_PLAY_CHUNK_BATCH_START = 0x0C
PKT_PLAY_DISCONNECT = 0x20
PKT_PLAY_SYNC_ENTITY_POSITION = 0x23
PKT_PLAY_UNLOAD_CHUNK = 0x25
PKT_PLAY_KEEP_ALIVE = 0x2B
PKT_PLAY_MAP_CHUNK = 0x2C
PKT_PLAY_LOGIN = 0x30
PKT_PLAY_ABILITIES = 0x3E
PKT_PLAY_PLAYER_REMOVE = 0x43
PKT_PLAY_PLAYER_INFO = 0x44
PKT_PLAY_POSITION = 0x46
PKT_PLAY_ENTITY_HEAD_ROTATION = 0x51
PKT_PLAY_SERVER_DATA = 0x54
PKT_PLAY_SET_CHUNK_CACHE_CENTER = 0x5C
PKT_PLAY_SPAWN_POSITION = 0x5F
PKT_PLAY_ENTITY_METADATA = 0x61
PKT_PLAY_EXPERIENCE = 0x65
PKT_PLAY_UPDATE_HEALTH = 0x66
PKT_PLAY_UPDATE_TIME = 0x6F
PKT_PLAY_SYSTEM_CHAT = 0x77
PKT_PLAY_REMOVE_ENTITIES = 0x4B

# Play state, serverbound.
PKT_PLAY_KEEP_ALIVE_RESPONSE = 0x19
PKT_PLAY_CHUNK_BATCH_RECEIVED = 0x0A
PKT_PLAY_TELEPORT_CONFIRM = 0x00
PKT_PLAY_CHAT_MESSAGE = 0x08
PKT_PLAY_POSITION_SB = 0x1D
PKT_PLAY_POSITION_LOOK_SB = 0x1E
PKT_PLAY_LOOK_SB = 0x1F
PKT_PLAY_BLOCK_DIG = 0x28
PKT_PLAY_BLOCK_PLACE = 0x3F

MOCK_PLAYER_ID = "0f9b0e00-0000-4000-8000-000000000000"

PASSED = 0
FAILED = 0


def check(condition: bool, label: str, detail: str = "") -> bool:
    global PASSED, FAILED
    if condition:
        PASSED += 1
        print(f"PASS: {label}")
    else:
        FAILED += 1
        print(f"FAIL: {label}" + (f" ({detail})" if detail else ""))
    return bool(condition)


# ── protocol encoding ───────────────────────────────────────────────────────────
def write_varint(value: int) -> bytes:
    out = bytearray()
    value &= 0xFFFFFFFF
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def read_varint(data: bytes, start: int = 0) -> tuple[int, int]:
    value = 0
    position = 0
    index = start
    while True:
        byte = data[index]
        index += 1
        value |= (byte & 0x7F) << position
        if not byte & 0x80:
            return value, index - start
        position += 7


def write_string(text: str) -> bytes:
    raw = text.encode()
    return write_varint(len(raw)) + raw


def pack_position(x: int, y: int, z: int) -> int:
    """Packed position: x in bits 38..63, z in bits 12..37, y in bits 0..11."""
    return ((x & 0x3FFFFFF) << 38) | ((z & 0x3FFFFFF) << 12) | (y & 0xFFF)


def read_string(data: bytes, start: int = 0) -> tuple[str, int]:
    length, size = read_varint(data, start)
    text = data[start + size : start + size + length].decode()
    return text, size + length


def server_id_hash(secret: bytes, public_key: bytes) -> str:
    """Java's `new BigInteger(sha1(secret ++ publicKey)).toString(16)`.

    With an empty server id, which is what vanilla sends. The digest is read as a
    *signed* big-endian integer, so a digest whose first bit is set is negative
    to Java and prints as a two's-complement value with a leading `-`; leading
    zero bytes are dropped. `int.from_bytes(..., signed=True)` plus `format(x)`
    reproduces `BigInteger.toString(16)` exactly. The dash-separated form that
    looks like a session id is the pre-1.7 format, which no client has ever sent
    to a modern session server.
    """
    digest = hashlib.sha1(secret + public_key).digest()
    return format(int.from_bytes(digest, "big", signed=True), "x")


class Client:
    """Minimal vanilla client: framing, optional zlib, optional AES/CFB8."""

    def __init__(self, host: str, port: int, timeout: float = 10.0) -> None:
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.encryptor = None
        self.decryptor = None
        self.compression: int | None = None

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass

    def _recv(self, count: int) -> bytes:
        buf = bytearray()
        while len(buf) < count:
            chunk = self.sock.recv(count - len(buf))
            if not chunk:
                raise EOFError("connection closed by server")
            buf.extend(chunk)
        raw = bytes(buf)
        return self.decryptor.update(raw) if self.decryptor else raw

    def _send(self, data: bytes) -> None:
        self.sock.sendall(self.encryptor.update(data) if self.encryptor else data)

    def enable_encryption(self, secret: bytes) -> None:
        cipher = Cipher(algorithms.AES(secret), modes.CFB8(secret))
        self.encryptor = cipher.encryptor()
        self.decryptor = cipher.decryptor()

    def read_varint(self) -> int:
        value = 0
        position = 0
        while True:
            byte = self._recv(1)[0]
            value |= (byte & 0x7F) << position
            if not byte & 0x80:
                return value
            position += 7

    def write_packet(self, packet_id: int, body: bytes = b"") -> None:
        data = write_varint(packet_id) + body
        if self.compression is None:
            self._send(write_varint(len(data)) + data)
            return

        if len(data) >= self.compression:
            inner = write_varint(len(data)) + zlib.compress(data)
        else:
            inner = write_varint(0) + data
        self._send(write_varint(len(inner)) + inner)

    def read_packet(self) -> tuple[int, bytes]:
        raw = self._recv(self.read_varint())
        if self.compression is not None:
            data_length, size = read_varint(raw)
            raw = zlib.decompress(raw[size:]) if data_length else raw[size:]
        packet_id, size = read_varint(raw)
        return packet_id, raw[size:]


def handshake(client: Client, host: str, port: int, next_state: int) -> None:
    body = (
        write_varint(PROTOCOL_VERSION)
        + write_string(host)
        + struct.pack(">H", port)
        + write_varint(next_state)
    )
    client.write_packet(PKT_HANDSHAKE, body)


def login_start(client: Client, name: str) -> uuid.UUID:
    player_uuid = uuid.uuid4()
    client.write_packet(PKT_LOGIN_START, write_string(name) + player_uuid.bytes)
    return player_uuid


def read_set_compression(client: Client) -> int:
    packet_id, body = client.read_packet()
    if not check(packet_id == PKT_SET_COMPRESSION, "set compression is sent after login",
                 f"got packet id {packet_id:#04x}"):
        raise AssertionError("missing Set Compression")
    threshold, _ = read_varint(body)
    client.compression = threshold
    return threshold


def read_login_success(client: Client) -> tuple[str, str, int]:
    packet_id, body = client.read_packet()
    if not check(packet_id == PKT_LOGIN_SUCCESS, "login success arrives after compression",
                 f"got packet id {packet_id:#04x}"):
        raise AssertionError("missing Login Success")
    player_uuid = str(uuid.UUID(bytes=body[:16]))
    username, size = read_string(body, 16)
    properties, _ = read_varint(body, 16 + size)
    return player_uuid, username, properties


def client_information() -> bytes:
    """ClientInformation 1.21.11: en_us, view distance 12, no chat flags,
    colors on, all skin parts, right-handed, no filtering, listed."""
    return (
        write_string("en_us")
        + bytes([12])
        + write_varint(0)
        + bytes([1, 0x7F])
        + write_varint(1)
        + bytes([0, 1])
        + write_varint(0)
    )


def complete_configuration(client: Client) -> int:
    """Drive the Configuration state like a vanilla client: echo `minecraft:core`,
    report client settings, gather registries and tags until Finish. Returns the
    number of registry packets the server sent."""
    while True:
        packet_id, body = client.read_packet()
        if packet_id in (PKT_CONFIG_CUSTOM_PAYLOAD, PKT_CONFIG_FEATURE_FLAGS):
            continue
        if not check(packet_id == PKT_CONFIG_KNOWN_PACKS,
                     "configuration opens with known packs", f"id {packet_id:#04x}"):
            raise AssertionError("missing ClientboundKnownPacks")
        count, size = read_varint(body)
        packs = []
        for _ in range(count):
            namespace, namespace_size = read_string(body, size)
            pack_id, id_size = read_string(body, size + namespace_size)
            version, version_size = read_string(body, size + namespace_size + id_size)
            size += namespace_size + id_size + version_size
            packs.append((namespace, pack_id, version))
        if not check(packs == [("minecraft", "core", "1.21.11")],
                     "server offers the core data pack", str(packs)):
            raise AssertionError("unexpected known packs")
        client.write_packet(
            PKT_CONFIG_SELECT_PACKS,
            write_varint(count)
            + write_string("minecraft")
            + write_string("core")
            + write_string("1.21.11"),
        )
        client.write_packet(PKT_CONFIG_CLIENT_INFO, client_information())
        break

    registries = 0
    while True:
        packet_id, _body = client.read_packet()
        if packet_id == PKT_CONFIG_REGISTRY_DATA:
            registries += 1
        elif packet_id in (PKT_CONFIG_UPDATE_TAGS, PKT_CONFIG_CUSTOM_PAYLOAD):
            continue
        elif packet_id == PKT_CONFIG_FINISH:
            client.write_packet(PKT_CONFIG_ACK)
            return registries
        else:
            check(False, "only expected configuration packets arrive",
                  f"id {packet_id:#04x}")
            raise AssertionError("bad configuration packet")


def parse_chunk_heightmaps(body: bytes) -> tuple[int, int, tuple[int, ...]]:
    """Reads a map_chunk packet's WORLD_SURFACE heightmap: returns the chunk
    coordinates and the 256 per-column surface heights."""
    chunk_x = struct.unpack(">i", body[0:4])[0]
    chunk_z = struct.unpack(">i", body[4:8])[0]
    count, consumed = read_varint(body, 8)
    size = 8 + consumed
    world_surface: tuple[int, ...] = ()
    for _ in range(count):
        map_type, consumed = read_varint(body, size)
        size += consumed
        length, consumed = read_varint(body, size)
        size += consumed
        words = struct.unpack(f">{length}q", body[size : size + 8 * length])
        size += 8 * length
        if map_type == 1:  # WORLD_SURFACE
            heights = []
            for word in words:
                word &= 0xFFFFFFFFFFFFFFFF
                for shift in range(0, 64, 9):
                    heights.append(word >> shift & 0x1FF)
            world_surface = tuple(heights[:256])
    return chunk_x, chunk_z, world_surface


def drive_into_play(client: Client, name: str, expected_entity_id: int | None = 0):
    """Consume the spawn sequence of the Play state: the login packet, the 3x3
    chunk batch, the first teleport and the health/welcome line. Acknowledges
    the chunk batch, confirms the teleport and answers keep-alives like a
    vanilla client would. Returns `(entity_id, position)`."""
    chunks = 0
    login = position = health = welcome = None
    acked = False
    surfaces = {}
    mob_spawns = {}
    while chunks < 9 or None in (login, position, health, welcome):
        packet_id, body = client.read_packet()
        if packet_id == PKT_PLAY_LOGIN:
            login = body
        elif packet_id == PKT_PLAY_MAP_CHUNK:
            chunks += 1
            chunk_x, chunk_z, heights = parse_chunk_heightmaps(body)
            if heights:
                surfaces[(chunk_x, chunk_z)] = heights
        elif packet_id == PKT_PLAY_SPAWN_ENTITY:
            entity_id, size = read_varint(body)
            entity_type, _ = read_varint(body, size + 16)
            if entity_type != 155:  # 155 is minecraft:player
                mob_spawns[entity_id] = entity_type
        elif packet_id == PKT_PLAY_CHUNK_BATCH_START:
            pass
        elif packet_id == PKT_PLAY_POSITION:
            position = body
            client.write_packet(PKT_PLAY_TELEPORT_CONFIRM, b"\x00")
        elif packet_id == PKT_PLAY_UPDATE_HEALTH:
            health = body
        elif packet_id == PKT_PLAY_SYSTEM_CHAT:
            welcome = body
        elif packet_id == PKT_PLAY_KEEP_ALIVE:
            client.write_packet(PKT_PLAY_KEEP_ALIVE_RESPONSE, body)
        elif packet_id == PKT_PLAY_DISCONNECT:
            check(False, "server does not disconnect during play", repr(body[:80]))
            raise AssertionError("kicked in play")
        # Spawn position, tab list, abilities, experience, time, server data,
        # chunk-batch finished: acknowledged silently.

        if chunks >= 9 and not acked:
            client.write_packet(PKT_PLAY_CHUNK_BATCH_RECEIVED, struct.pack("<f", 10.0))
            acked = True

    check(chunks == 9, "the client receives a 3x3 chunk batch", f"{chunks} chunks")

    (entity_id,) = struct.unpack(">i", login[:4])
    if expected_entity_id is not None:
        check(entity_id == expected_entity_id,
              f"play login assigns entity id {expected_entity_id}", str(entity_id))

    # Generated terrain, not one flat template: the 3x3 heightmaps must differ.
    unique_surfaces = {heights for heights in surfaces.values()}
    check(len(surfaces) == 9, "all nine chunks carry a heightmap", str(len(surfaces)))
    check(len(unique_surfaces) > 1, "chunk heightmaps differ (generated terrain)")

    # teleport: varint id, then three doubles; little-endian for the f32 health below.
    pos = struct.unpack(">3d", position[1:25])
    teleport_flags = struct.unpack(">i", position[-4:])[0]
    check((pos[0], pos[2]) == (8.5, 8.5), "the first teleport lands on spawn x/z", str(pos))
    check(-55.0 <= pos[1] <= 121.0 and float(pos[1]).is_integer(),
          "the player stands on the generated surface", str(pos))
    check(teleport_flags == 0, "the first teleport is absolute", str(teleport_flags))

    health_value = struct.unpack(">f", health[:4])[0]
    check(health_value == 20.0, "the player starts at full health", str(health_value))

    check(name.encode() in welcome and b"Welcome" in welcome,
          "a chat welcome greets the player by name")
    return entity_id, pos, mob_spawns


def chat_echo(client: Client, name: str, message: str) -> None:
    """A chat_message round trip: the server must echo it back with the player's
    name. Chat requires no signature under loadstone's relaxed chat policy."""
    body = (
        write_string(message)
        + struct.pack(">qq", 0, 0)  # timestamp, salt
        + b"\x00"                   # no signature
        + write_varint(0)           # offset
        + bytes(3)                  # acknowledged
        + b"\x00"                   # checksum
    )
    client.write_packet(PKT_PLAY_CHAT_MESSAGE, body)
    while True:
        packet_id, body = client.read_packet()
        if packet_id == PKT_PLAY_KEEP_ALIVE:
            client.write_packet(PKT_PLAY_KEEP_ALIVE_RESPONSE, body)
            continue
        if packet_id == PKT_PLAY_SYSTEM_CHAT and message.encode() in body:
            check(name.encode() in body, "the server echoes the chat back to the player")
            return
        if packet_id == PKT_PLAY_DISCONNECT:
            check(False, "server does not disconnect during play", repr(body[:80]))
            raise AssertionError("kicked in play")


def block_round_trip(client: Client, surface_y: int) -> None:
    """Dig the surface block below the spawn and place a cobblestone on its
    empty spot; the server must answer each with a block_change packet matching
    the new state. `surface_y` is the generated grass block the player stands on."""
    # Break the grass block the spawn stands on.
    client.write_packet(
        PKT_PLAY_BLOCK_DIG,
        write_varint(2)
        + struct.pack(">q", pack_position(8, surface_y, 8))
        + bytes([1])
        + write_varint(1),
    )
    packet_id, body = await_block_change_packet(client)
    if not check(packet_id == PKT_PLAY_BLOCK_CHANGE, "digging answers with a block change",
                 f"id {packet_id:#04x}"):
        raise AssertionError("missing block change after dig")

    (location,) = struct.unpack(">q", body[:8])
    state, _ = read_varint(body, 8)
    check(location == pack_position(8, surface_y, 8),
          f"the broken block is the surface at (8,{surface_y},8)", hex(location))
    check(state == 0, "a dug block becomes air", str(state))

    # Place on the top face of the now-empty spot: the block lands above it.
    client.write_packet(
        PKT_PLAY_BLOCK_PLACE,
        write_varint(0)                                       # hand
        + struct.pack(">q", pack_position(8, surface_y, 8))   # clicked location
        + write_varint(1)                                     # top face
        + struct.pack(">3f", 0.5, 1.0, 0.5)                   # cursor
        + b"\x00\x00"                                        # inside block, world border
        + write_varint(2),                                   # sequence
    )
    packet_id, body = await_block_change_packet(client)
    if not check(packet_id == PKT_PLAY_BLOCK_CHANGE,
                 "placing answers with a block change", f"id {packet_id:#04x}"):
        raise AssertionError("missing block change after place")

    (location,) = struct.unpack(">q", body[:8])
    state, _ = read_varint(body, 8)
    check(location == pack_position(8, surface_y + 1, 8),
          f"the placed block lands at (8,{surface_y + 1},8)", hex(location))
    check(state == 14, "a placed block becomes cobblestone", str(state))


# ── checks ──────────────────────────────────────────────────────────────────────
# ── two-player shared-world helpers ─────────────────────────────────────────────
def next_play_packet(client: Client) -> tuple[int, bytes]:
    """Reads the next Play packet, answering keep-alives and refusing disconnects."""
    while True:
        packet_id, body = client.read_packet()
        if packet_id == PKT_PLAY_KEEP_ALIVE:
            client.write_packet(PKT_PLAY_KEEP_ALIVE_RESPONSE, body)
            continue
        if packet_id == PKT_PLAY_DISCONNECT:
            check(False, "server does not disconnect during play", repr(body[:80]))
            raise AssertionError("kicked in play")
        return packet_id, body


def await_chunk_packet(client: Client, wanted: int) -> tuple[int, bytes]:
    """Reads Play packets until `wanted` arrives, skipping the entity traffic
    the mob ticker interleaves with chunk streaming."""
    entity_traffic = {
        PKT_PLAY_SPAWN_ENTITY,
        PKT_PLAY_SYNC_ENTITY_POSITION,
        PKT_PLAY_REMOVE_ENTITIES,
        PKT_PLAY_ENTITY_METADATA,
        PKT_PLAY_BUNDLE_DELIMITER,
        PKT_PLAY_ENTITY_HEAD_ROTATION,
    }
    while True:
        packet_id, body = next_play_packet(client)
        if packet_id == wanted:
            return packet_id, body
        if packet_id not in entity_traffic:
            return packet_id, body


def await_spawn(client: Client, expected_uuid: uuid.UUID) -> int:
    """Reads until the given player's spawn_entity appears, returning their id."""
    while True:
        packet_id, body = next_play_packet(client)
        if packet_id != PKT_PLAY_SPAWN_ENTITY:
            continue
        entity_id, size = read_varint(body)
        spawn_uuid = uuid.UUID(bytes=body[size:size + 16])
        entity_type, _ = read_varint(body, size + 16)
        if spawn_uuid == expected_uuid:
            check(entity_type == 155, "the other player spawns as minecraft:player (155)",
                  str(entity_type))
            return entity_id


def await_sync(client: Client, entity_id: int) -> tuple[float, float, float]:
    """Reads until an entity sync for `entity_id`, returning the synced position."""
    while True:
        packet_id, body = next_play_packet(client)
        if packet_id != PKT_PLAY_SYNC_ENTITY_POSITION:
            continue
        synced_id, size = read_varint(body)
        if synced_id != entity_id:
            continue
        x, y, z = struct.unpack(">3d", body[size:size + 24])
        return x, y, z


def await_block_change(client: Client) -> int:
    """Reads until a block_change, returning the packed location."""
    packet_id, body = await_block_change_packet(client)
    (location,) = struct.unpack(">q", body[:8])
    return location


def await_block_change_packet(client: Client) -> tuple[int, bytes]:
    """Reads Play packets until a block_change arrives, skipping the mob and
    keep-alive traffic the entity ticker interleaves."""
    while True:
        packet_id, body = next_play_packet(client)
        if packet_id == PKT_PLAY_BLOCK_CHANGE:
            return packet_id, body


def check_mobs(host: str, port: int) -> None:
    """A joining player is told about the starting mob population and sees it
    move. Mobs must use the `minecraft:entity_type` ids and never the player id."""
    client = Client(host, port)
    try:
        handshake(client, host, port, 2)
        login_start(client, "MobWatch")
        read_set_compression(client)
        read_login_success(client)
        client.write_packet(PKT_LOGIN_ACKNOWLEDGED)
        complete_configuration(client)
        _, _, spawns = drive_into_play(client, "MobWatch", expected_entity_id=None)

        spawned = dict(spawns)
        moved = set()
        # The server ticks at 20 Hz; wait for the whole population plus one move.
        while len(spawned) < 7 or not moved:
            packet_id, body = next_play_packet(client)
            if packet_id == PKT_PLAY_SPAWN_ENTITY:
                entity_id, size = read_varint(body)
                entity_type, _ = read_varint(body, size + 16)
                if entity_type != 155:
                    spawned[entity_id] = entity_type
            elif packet_id == PKT_PLAY_SYNC_ENTITY_POSITION:
                entity_id, _ = read_varint(body)
                moved.add(entity_id)

        expected = [26, 30, 100, 111, 150]  # chicken, cow, pig, sheep, zombie
        distinct = sorted(set(spawned.values()))
        check(distinct == expected,
              "the starting mobs use the 1.21.11 entity_type ids", str(distinct))
        check(moved and moved <= set(spawned),
              "mob movement is streamed for the spawned entities",
              f"moved {sorted(moved)} of {sorted(spawned)}")
    finally:
        client.close()


def await_player_remove(client: Client) -> bool:
    """Reads until a player_remove, returning whether one arrived."""
    while True:
        packet_id, _body = next_play_packet(client)
        if packet_id == PKT_PLAY_PLAYER_REMOVE:
            return True


def check_two_players(host: str, port: int) -> None:
    """Two clients share one world: each sees the other spawn, move, edit blocks
    and leave."""
    alice = Client(host, port)
    bob = Client(host, port)
    try:
        handshake(alice, host, port, 2)
        login_start(alice, "Alice")
        read_set_compression(alice)
        read_login_success(alice)
        alice.write_packet(PKT_LOGIN_ACKNOWLEDGED)
        complete_configuration(alice)
        alice_entity, _alice_pos, _alice_mobs = drive_into_play(alice, "Alice", expected_entity_id=None)

        handshake(bob, host, port, 2)
        bob_uuid = login_start(bob, "Bob")
        read_set_compression(bob)
        read_login_success(bob)
        bob.write_packet(PKT_LOGIN_ACKNOWLEDGED)
        complete_configuration(bob)
        bob_entity_login, bob_position, _bob_mobs = drive_into_play(bob, "Bob", expected_entity_id=None)

        check(alice_entity != bob_entity_login,
              "players receive distinct entity ids", f"{alice_entity} == {bob_entity_login}")

        # Alice is introduced to Bob's entity.
        bob_entity = await_spawn(alice, bob_uuid)
        check(bob_entity == bob_entity_login,
              "the spawned entity matches Bob's login entity id",
              f"{bob_entity} != {bob_entity_login}")

        # Bob's movement is synced to Alice.
        bob.write_packet(PKT_PLAY_POSITION_SB, struct.pack(">3d", 9.0, 65.0, 8.5) + b"\x01")
        synced = await_sync(alice, bob_entity)
        check(synced == (9.0, 65.0, 8.5), "movement reaches the other player", str(synced))

        # Bob's block edit is shared with Alice. (7, surface, 8) is untouched grass.
        surface = int(bob_position[1]) - 1
        bob.write_packet(
            PKT_PLAY_BLOCK_DIG,
            write_varint(2) + struct.pack(">q", pack_position(7, surface, 8))
            + bytes([1]) + write_varint(1),
        )
        shared = await_block_change(alice)
        check(shared == pack_position(7, surface, 8),
              "a block edit is shared between players", hex(shared))

        # Bob leaves; Alice is told to drop him.
        bob.close()
        check(await_player_remove(alice),
              "a leaving player is removed from the other client")
    finally:
        alice.close()
        bob.close()


def check_chunk_streaming(host: str, port: int) -> None:
    """Walking into a neighbouring chunk streams the columns that entered view
    and unloads the ones that fell out of it."""
    client = Client(host, port)
    try:
        handshake(client, host, port, 2)
        login_start(client, "Walker")
        read_set_compression(client)
        read_login_success(client)
        client.write_packet(PKT_LOGIN_ACKNOWLEDGED)
        complete_configuration(client)
        drive_into_play(client, "Walker", expected_entity_id=None)

        # Walk east, from chunk (0,0) into chunk (1,0).
        client.write_packet(PKT_PLAY_POSITION_SB, struct.pack(">3d", 24.5, 65.0, 8.5) + b"\x01")

        packet_id, body = await_chunk_packet(client, PKT_PLAY_SET_CHUNK_CACHE_CENTER)
        if not check(packet_id == PKT_PLAY_SET_CHUNK_CACHE_CENTER,
                     "crossing a chunk border announces the new centre",
                     f"id {packet_id:#04x}"):
            return
        center_x, size = read_varint(body)
        center_z, _ = read_varint(body, size)
        check((center_x, center_z) == (1, 0), "the view centre follows the player",
              f"({center_x}, {center_z})")

        packet_id, _ = await_chunk_packet(client, PKT_PLAY_CHUNK_BATCH_START)
        check(packet_id == PKT_PLAY_CHUNK_BATCH_START, "new chunks arrive as a batch",
              f"id {packet_id:#04x}")

        loaded = []
        batch_size = None
        while True:
            packet_id, body = next_play_packet(client)
            if packet_id == PKT_PLAY_MAP_CHUNK:
                loaded.append(struct.unpack(">i", body[:4])[0])
            elif packet_id == PKT_PLAY_CHUNK_BATCH_FINISHED:
                batch_size, _ = read_varint(body)
                break
        check(batch_size == 3 and loaded == [2, 2, 2],
              "the three columns that entered view are streamed",
              f"size {batch_size} xs {loaded}")

        unloaded = []
        for _ in range(3):
            packet_id, body = await_chunk_packet(client, PKT_PLAY_UNLOAD_CHUNK)
            if not check(packet_id == PKT_PLAY_UNLOAD_CHUNK,
                         "columns that left view are unloaded", f"id {packet_id:#04x}"):
                return
            chunk_z = struct.unpack(">i", body[:4])[0]
            chunk_x = struct.unpack(">i", body[4:8])[0]
            unloaded.append((chunk_x, chunk_z))
        check(sorted(unloaded) == [(-1, -1), (-1, 0), (-1, 1)],
              "the whole column band that left view is unloaded", str(unloaded))
    finally:
        client.close()


def check_status(host: str, port: int, motd: str) -> None:
    client = Client(host, port)
    try:
        handshake(client, host, port, 1)
        client.write_packet(PKT_STATUS_REQUEST)

        packet_id, body = client.read_packet()
        check(packet_id == PKT_STATUS_RESPONSE, "status response uses id 0x00", f"id {packet_id:#04x}")
        raw, _ = read_string(body)
        status = json.loads(raw)

        check(status["version"]["protocol"] == PROTOCOL_VERSION,
              f"status reports protocol {PROTOCOL_VERSION}", str(status["version"]))
        check(status["version"]["name"] == MINECRAFT_VERSION,
              f"status reports version {MINECRAFT_VERSION}", str(status["version"]))
        check(status["description"]["text"] == motd,
              "status carries the configured MOTD", str(status["description"]))
        check("enforcesSecureChat" in status, "status carries the 1.19+ chat flag")

        payload = 0x0123456789ABCDEF
        client.write_packet(PKT_PING, struct.pack(">q", payload))
        packet_id, body = client.read_packet()
        pong = struct.unpack(">q", body[:8])[0]
        check(packet_id == PKT_PONG and pong == payload, "ping is echoed as pong",
              f"id {packet_id:#04x} payload {pong}")
    finally:
        client.close()


def check_offline_login(host: str, port: int, name: str) -> None:
    client = Client(host, port)
    try:
        handshake(client, host, port, 2)
        sent_uuid = login_start(client, name)

        read_set_compression(client)
        player_uuid, username, properties = read_login_success(client)

        check(username == name, "offline login keeps the requested username", username)
        check(player_uuid == str(sent_uuid), "offline login echoes the client uuid",
              f"{player_uuid} != {sent_uuid}")
        check(properties == 0, "offline login sends no profile properties", str(properties))

        client.write_packet(PKT_LOGIN_ACKNOWLEDGED)
        registries = complete_configuration(client)
        check(registries == 23, "all synchronized registries are delivered",
              f"{registries} registries")
        _, position, _mobs = drive_into_play(client, name)
        chat_echo(client, name, "hello from live check")
        block_round_trip(client, int(position[1]) - 1)
    finally:
        client.close()


def check_online_login(host: str, port: int, name: str, session: "MockSession") -> None:
    client = Client(host, port)
    try:
        handshake(client, host, port, 2)
        login_start(client, name)

        packet_id, body = client.read_packet()
        if not check(packet_id == PKT_ENCRYPTION_REQUEST,
                     "online mode asks for encryption", f"id {packet_id:#04x}"):
            return

        server_id, size = read_string(body)
        key_len, key_size = read_varint(body, size)
        public_key = body[size + key_size : size + key_size + key_len]
        token_len, token_size = read_varint(body, size + key_size + key_len)
        token_start = size + key_size + key_len + token_size
        verify_token = body[token_start : token_start + token_len]
        should_authenticate = body[token_start + token_len] == 1

        check(server_id == "", "encryption request omits the legacy server id", server_id)
        check(should_authenticate, "encryption request asks the client to authenticate")
        check(len(verify_token) == 4, "verify token is 4 bytes, like vanilla's", str(len(verify_token)))

        public = serialization.load_der_public_key(public_key)
        secret = os.urandom(16)
        encrypted_secret = public.encrypt(secret, padding.PKCS1v15())
        # The token is echoed **encrypted**, the same way the session key is: a
        # real client never returns it in the clear, so a server that accepts
        # clear text would look fine here and reject every real login.
        encrypted_token = public.encrypt(verify_token, padding.PKCS1v15())
        expected_server_id = server_id_hash(secret, public_key)

        client.write_packet(
            PKT_ENCRYPTION_RESPONSE,
            write_varint(len(encrypted_secret))
            + encrypted_secret
            + write_varint(len(encrypted_token))
            + encrypted_token,
        )
        client.enable_encryption(secret)

        read_set_compression(client)
        player_uuid, username, properties = read_login_success(client)

        check(player_uuid == MOCK_PLAYER_ID, "online login uses the session server uuid", player_uuid)
        check(username == name, "online login uses the session server name", username)
        check(properties == 0, "profile properties are carried through", str(properties))

        client.write_packet(PKT_LOGIN_ACKNOWLEDGED)
        registries = complete_configuration(client)
        check(registries == 23, "all synchronized registries are delivered",
              f"{registries} registries")
        _, position, _mobs = drive_into_play(client, name)
        chat_echo(client, name, "hello from live check")
        block_round_trip(client, int(position[1]) - 1)
    finally:
        client.close()

    check(session.contacted(), "session server was contacted for verification")
    check(session.server_id() == expected_server_id,
          "client and server agree on the serverId hash",
          f"{session.server_id()} != {expected_server_id}")


def check_online_rejection(host: str, port: int, session: "MockSession") -> None:
    client = Client(host, port)
    try:
        handshake(client, host, port, 2)
        login_start(client, "Intruder")

        packet_id, body = client.read_packet()
        if not check(packet_id == PKT_ENCRYPTION_REQUEST,
                     "an unknown account still gets an encryption request",
                     f"id {packet_id:#04x}"):
            return

        _, size = read_string(body)
        key_len, key_size = read_varint(body, size)
        public_key = body[size + key_size : size + key_size + key_len]
        token_len, token_size = read_varint(body, size + key_size + key_len)
        token_start = size + key_size + key_len + token_size
        verify_token = body[token_start : token_start + token_len]

        public = serialization.load_der_public_key(public_key)
        secret = os.urandom(16)
        encrypted_secret = public.encrypt(secret, padding.PKCS1v15())
        # Encrypted like a real client's, so this flow reaches the session check
        # instead of being refused for a token it never echoed properly.
        encrypted_token = public.encrypt(verify_token, padding.PKCS1v15())

        client.write_packet(
            PKT_ENCRYPTION_RESPONSE,
            write_varint(len(encrypted_secret))
            + encrypted_secret
            + write_varint(len(encrypted_token))
            + encrypted_token,
        )
        client.enable_encryption(secret)

        packet_id, body = client.read_packet()
        if not check(packet_id == PKT_LOGIN_DISCONNECT,
                     "an unverified account is disconnected", f"id {packet_id:#04x}"):
            return
        reason, _ = read_string(body)
        check("Failed to verify username" in reason, "the disconnect explains the reason", reason)
    finally:
        client.close()


def check_online_token_mismatch(host: str, port: int) -> None:
    """A valid key exchange that does not echo the verify token must be refused."""
    client = Client(host, port)
    try:
        handshake(client, host, port, 2)
        login_start(client, "MockPlayer")

        packet_id, body = client.read_packet()
        if not check(packet_id == PKT_ENCRYPTION_REQUEST,
                     "a mismatched-token client still gets an encryption request",
                     f"id {packet_id:#04x}"):
            return

        _, size = read_string(body)
        key_len, key_size = read_varint(body, size)
        public_key = body[size + key_size : size + key_size + key_len]

        public = serialization.load_der_public_key(public_key)
        secret = os.urandom(16)
        encrypted_secret = public.encrypt(secret, padding.PKCS1v15())
        # Encrypted properly, but not the token the server sent.
        bogus_token = public.encrypt(bytes(4), padding.PKCS1v15())

        client.write_packet(
            PKT_ENCRYPTION_RESPONSE,
            write_varint(len(encrypted_secret))
            + encrypted_secret
            + write_varint(len(bogus_token))
            + bogus_token,
        )
        client.enable_encryption(secret)

        packet_id, body = client.read_packet()
        if not check(packet_id == PKT_LOGIN_DISCONNECT,
                     "a mismatched verify token is refused", f"id {packet_id:#04x}"):
            return
        reason, _ = read_string(body)
        check("Invalid verify token" in reason, "the refusal names the verify token", reason)
    finally:
        client.close()


# ── mock session server ─────────────────────────────────────────────────────────
class MockSession:
    """Answers hasJoined the way Mojang does, for one known username."""

    def __init__(self, accept_name: str, player_id: str) -> None:
        self.accept_name = accept_name
        self.player_id = player_id
        self.requests: list[dict[str, list[str]]] = []
        harness = self
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:  # noqa: N802 - name fixed by http.server
                query = parse_qs(urlparse(self.path).query)
                harness.requests.append(query)
                username = (query.get("username") or [""])[0]
                body = (
                    json.dumps({"id": outer.player_id, "name": username, "properties": []}).encode()
                    if username == outer.accept_name
                    else b""
                )
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args) -> None:
                pass

        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def contacted(self) -> bool:
        return bool(self.requests)

    def server_id(self) -> str:
        if not self.requests:
            return ""
        return (self.requests[0].get("serverId") or [""])[0]

    def stop(self) -> None:
        self.httpd.shutdown()
        self.httpd.server_close()


# ── server lifecycle ────────────────────────────────────────────────────────────
def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def wait_for_port(host: str, port: int, timeout: float = 15.0) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return True
        except OSError:
            time.sleep(0.1)
    return False


def spawn_server(binary: str, port: int, motd: str, online: bool, session_url: str | None):
    """Starts the server on a fixed seed in a throwaway world directory so the
    checks are reproducible and never touch a real save."""
    world_dir = tempfile.mkdtemp(prefix="loadstone-live-world-")
    # A scratch --dir keeps the `server.properties`, `eula.txt` and world this
    # check needs out of the checkout, and the online mode is passed explicitly
    # so the run does not depend on what that file happens to default to.
    scratch = tempfile.mkdtemp(prefix="loadstone-live-dir-")
    command = [
        binary,
        "--dir", scratch,
        "--accept-eula",
        "--bind", f"127.0.0.1:{port}",
        "--motd", motd,
        "--seed", "0",
        "--world", world_dir,
        "--save-interval", "0",
        "--online-mode=" + ("true" if online else "false"),
    ]
    if online:
        command += ["--sessionserver-url", session_url or ""]
    process = subprocess.Popen(
        command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
    )
    process.loadstone_world_dir = world_dir  # type: ignore[attr-defined]
    if not wait_for_port("127.0.0.1", port):
        process.kill()
        output = process.stdout.read() if process.stdout else ""
        shutil.rmtree(world_dir, ignore_errors=True)
        raise SystemExit(f"server did not start:\n{output}")
    return process


def check_persistence(process: subprocess.Popen) -> None:
    """Signals the server to save on shutdown, then inspects the Anvil output:
    a region file holding the generated terrain and the edits made above."""
    world_dir = getattr(process, "loadstone_world_dir", None)
    if not check(world_dir is not None, "the server runs with a scratch world directory"):
        return
    if not check(process.poll() is None, "the server is still running before shutdown"):
        return

    process.send_signal(signal.SIGINT)
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        check(False, "the server shuts down on interrupt")
        process.kill()
        return
    check(process.returncode == 0, "the server exits cleanly after saving",
          str(process.returncode))

    region_dir = os.path.join(world_dir, "region")
    regions = sorted(glob.glob(os.path.join(region_dir, "*.mca")))
    if not check(bool(regions), "shutdown wrote an Anvil region file", region_dir):
        return
    check(os.path.exists(os.path.join(world_dir, "loadstone.seed")),
          "the seed sidecar is written next to the regions")

    # The spawn is chunk (0,0), so its payload lives in slot 0 of r.0.0.mca.
    spawn_region = os.path.join(region_dir, "r.0.0.mca")
    if not check(os.path.exists(spawn_region), "the spawn region r.0.0.mca is written",
                 spawn_region):
        return
    with open(spawn_region, "rb") as handle:
        region = handle.read()
    if not check(len(region) >= 8192, "the region file has an 8 KiB header", str(len(region))):
        return
    offset = int.from_bytes(region[0:3], "big")
    if not check(offset != 0, "the spawn chunk slot points at a payload"):
        return
    start = offset * 4096
    length = int.from_bytes(region[start : start + 4], "big")
    compression = region[start + 4]
    payload = zlib.decompress(region[start + 5 : start + 4 + length])
    check(compression == 2, "region chunks are zlib compressed", str(compression))
    check(b"DataVersion" in payload, "the saved chunk carries a DataVersion")
    check(b"minecraft:grass_block" in payload, "the saved chunk holds generated terrain")
    check(b"minecraft:cobblestone" in payload, "the saved chunk holds a player edit")


def main() -> int:
    parser = argparse.ArgumentParser(description="Live protocol checks for a LoadstoneMC server.")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=25577, help="server port (default 25577)")
    parser.add_argument("--mode", choices=["offline", "online"], default="offline")
    parser.add_argument("--name", default="LiveCheck", help="username for the offline login")
    parser.add_argument("--mock-name", default="MockPlayer", help="username the mock session server accepts")
    parser.add_argument("--motd", default="LoadstoneMC live check")
    parser.add_argument("--binary", default="target/release/loadstone")
    parser.add_argument("--no-spawn", action="store_true", help="check an already running server")
    args = parser.parse_args()

    session = None
    process = None

    try:
        if args.mode == "online" and not args.no_spawn:
            session = MockSession(args.mock_name, MOCK_PLAYER_ID)

        if args.mode == "online" and args.no_spawn:
            # Online checks need the server pointed at the mock session server, which means spawning it here.
            print("FAIL: online checks spawn the server so it can use the mock session server")
            print("      run without --no-spawn, or point --binary at the build you want to check")
            return 2

        if not args.no_spawn:
            binary = args.binary
            if not os.path.exists(binary):
                print(f"FAIL: {binary} not found — build it with 'cargo build --release'")
                return 2
            process = spawn_server(
                binary,
                args.port,
                args.motd,
                online=args.mode == "online",
                session_url=session.url if session else None,
            )

        print(f"checking {args.host}:{args.port} in {args.mode} mode\n")

        check_status(args.host, args.port, args.motd)

        if args.mode == "offline":
            check_offline_login(args.host, args.port, args.name)
            check_chunk_streaming(args.host, args.port)
            check_two_players(args.host, args.port)
            check_mobs(args.host, args.port)
            if process is not None:
                check_persistence(process)
        elif session is not None:
            check_online_login(args.host, args.port, args.mock_name, session)
            check_online_rejection(args.host, args.port, session)
            check_online_token_mismatch(args.host, args.port)
    except (AssertionError, EOFError, OSError) as error:
        check(False, "checks ran to completion", str(error))
    finally:
        if process is not None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
            shutil.rmtree(getattr(process, "loadstone_world_dir", ""), ignore_errors=True)
        if session is not None:
            session.stop()

    print(f"\n{PASSED} passed, {FAILED} failed")
    return 0 if FAILED == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
