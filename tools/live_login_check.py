#!/usr/bin/env python3
"""Live client checks against a LoadstoneMC server.

Speaks the wire protocol itself, the way a vanilla client does, so the server is
verified over a real socket instead of in-process: server list ping, login
(offline and online modes, including the RSA key exchange with AES/CFB8
streaming and a Mojang-style `hasJoined` lookup against a local mock session
server), the full Configuration handshake, and the Play state where the client
spawns onto the flat world and chat is echoed back. Also covers the two
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
import os
import socket
import struct
import subprocess
import sys
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
PKT_PLAY_LOGIN = 0x2E
PKT_PLAY_SPAWN_POSITION = 0x5B
PKT_PLAY_CHUNK_BATCH_START = 0x0B
PKT_PLAY_MAP_CHUNK = 0x2A
PKT_PLAY_CHUNK_BATCH_FINISHED = 0x0A
PKT_PLAY_KEEP_ALIVE = 0x29
PKT_PLAY_POSITION = 0x44
PKT_PLAY_PLAYER_INFO = 0x42
PKT_PLAY_ABILITIES = 0x3C
PKT_PLAY_UPDATE_HEALTH = 0x62
PKT_PLAY_EXPERIENCE = 0x61
PKT_PLAY_UPDATE_TIME = 0x6B
PKT_PLAY_SERVER_DATA = 0x50
PKT_PLAY_SYSTEM_CHAT = 0x72
PKT_PLAY_DISCONNECT = 0x1E

# Play state, serverbound.
PKT_PLAY_KEEP_ALIVE_RESPONSE = 0x19
PKT_PLAY_CHUNK_BATCH_RECEIVED = 0x0A
PKT_PLAY_TELEPORT_CONFIRM = 0x00
PKT_PLAY_CHAT_MESSAGE = 0x08

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


def read_string(data: bytes, start: int = 0) -> tuple[str, int]:
    length, size = read_varint(data, start)
    text = data[start + size : start + size + length].decode()
    return text, size + length


def server_id_hash(secret: bytes, public_key: bytes) -> str:
    """Java's Crypt.digestData: SHA-1(secret ++ publicKey), dash after every byte."""
    digest = hashlib.sha1(secret + public_key).hexdigest()
    return "".join(f"{digest[i:i + 2]}-" for i in range(0, len(digest), 2))


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


def drive_into_play(client: Client, name: str) -> None:
    """Consume the spawn sequence of the Play state: the login packet, the 3x3
    chunk batch, the first teleport and the health/welcome line. Acknowledges
    the chunk batch, confirms the teleport and answers keep-alives like a
    vanilla client would."""
    chunks = 0
    login = position = health = welcome = None
    acked = False
    while chunks < 9 or None in (login, position, health, welcome):
        packet_id, body = client.read_packet()
        if packet_id == PKT_PLAY_LOGIN:
            login = body
        elif packet_id == PKT_PLAY_MAP_CHUNK:
            chunks += 1
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
    check(entity_id == 0, "play login assigns entity id 0", str(entity_id))

    # teleport: varint id, then three doubles; little-endian for the f32 health below.
    pos = struct.unpack(">3d", position[1:25])
    teleport_flags = struct.unpack(">i", position[-4:])[0]
    check(pos == (8.5, 65.0, 8.5), "the first teleport lands on spawn", str(pos))
    check(teleport_flags == 0, "the first teleport is absolute", str(teleport_flags))

    health_value = struct.unpack(">f", health[:4])[0]
    check(health_value == 20.0, "the player starts at full health", str(health_value))

    check(name.encode() in welcome and b"Welcome" in welcome,
          "a chat welcome greets the player by name")


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


# ── checks ──────────────────────────────────────────────────────────────────────
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
        drive_into_play(client, name)
        chat_echo(client, name, "hello from live check")
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
        check(len(verify_token) == 16, "verify token is 16 bytes", str(len(verify_token)))

        public = serialization.load_der_public_key(public_key)
        secret = os.urandom(16)
        encrypted_secret = public.encrypt(secret, padding.PKCS1v15())
        expected_server_id = server_id_hash(secret, public_key)

        client.write_packet(
            PKT_ENCRYPTION_RESPONSE,
            write_varint(len(encrypted_secret))
            + encrypted_secret
            + write_varint(len(verify_token))
            + verify_token,
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
        drive_into_play(client, name)
        chat_echo(client, name, "hello from live check")
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

        client.write_packet(
            PKT_ENCRYPTION_RESPONSE,
            write_varint(len(encrypted_secret))
            + encrypted_secret
            + write_varint(len(verify_token))
            + verify_token,
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
        bogus_token = bytes(16)

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
    command = [binary, "--bind", f"127.0.0.1:{port}", "--motd", motd]
    if online:
        command += ["--online-mode", "--sessionserver-url", session_url or ""]
    process = subprocess.Popen(
        command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
    )
    if not wait_for_port("127.0.0.1", port):
        process.kill()
        output = process.stdout.read() if process.stdout else ""
        raise SystemExit(f"server did not start:\n{output}")
    return process


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
        if session is not None:
            session.stop()

    print(f"\n{PASSED} passed, {FAILED} failed")
    return 0 if FAILED == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
