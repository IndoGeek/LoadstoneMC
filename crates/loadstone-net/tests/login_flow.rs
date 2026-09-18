//! End-to-end login tests: a simulated vanilla client handshakes, logs in
//! (offline and online modes), negotiates AES/CFB8 encryption and zlib
//! compression, and verifies the server's Login Success.
//!
//! Online mode uses a local mock "Mojang session server" so no real accounts
//! or internet are required.

use std::io::Write as _;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use loadstone_net::auth::compute_server_id;
use loadstone_net::{run_connection, Cfb8, ConnectionConfig};
use loadstone_protocol::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginAcknowledged, LoginStart, LoginSuccess,
    SetCompression,
};
use loadstone_protocol::packets::Handshake;
use loadstone_protocol::write_varint;
use loadstone_protocol::{Packet, PacketReader, PROTOCOL_VERSION};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

const MOCK_PLAYER_ID: &str = "0f9b0e00-0000-4000-8000-000000000000";

/// A vanilla-behaving test client with optional streaming encryption.
struct Client {
    stream: TcpStream,
    out: Option<Cfb8>,
    inp: Option<Cfb8>,
    compression: Option<i32>,
}

impl Client {
    async fn connect(addr: &str) -> Self {
        let stream = TcpStream::connect(addr).await.unwrap();
        Self {
            stream,
            out: None,
            inp: None,
            compression: None,
        }
    }

    fn enable_encryption(&mut self, secret: [u8; 16]) {
        self.out = Some(Cfb8::new(&secret, &secret, true));
        self.inp = Some(Cfb8::new(&secret, &secret, false));
    }

    async fn read_byte_decrypted(&mut self) -> Option<u8> {
        let raw = self.stream.read_u8().await.ok()?;
        let byte = match &mut self.inp {
            Some(cipher) => {
                let mut b = [raw];
                cipher.transform(&mut b);
                b[0]
            }
            None => raw,
        };
        Some(byte)
    }

    async fn read_varint(&mut self) -> u32 {
        let mut value = 0u32;
        let mut position = 0u32;
        loop {
            let byte = self.read_byte_decrypted().await.expect("eof in varint");
            value |= ((byte & 0x7F) as u32) << position;
            if byte & 0x80 == 0 {
                return value;
            }
            position += 7;
        }
    }

    async fn recv_packet(&mut self) -> (i32, Vec<u8>) {
        let len = self.read_varint().await as usize;
        let mut raw = vec![0u8; len];
        self.stream.read_exact(&mut raw).await.unwrap();
        if let Some(cipher) = &mut self.inp {
            cipher.transform(&mut raw);
        }

        let mut data = match self.compression {
            None => raw,
            Some(_) => {
                let (data_len, n) = read_varint(&raw);
                let rest = raw[n..].to_vec();
                if data_len == 0 {
                    rest
                } else {
                    let mut out = Vec::with_capacity(data_len as usize);
                    flate2::write::ZlibDecoder::new(&mut out)
                        .write_all(&rest)
                        .unwrap();
                    out
                }
            }
        };

        let (id, n) = read_varint(&data);
        data.drain(..n);
        (id as i32, data)
    }

    async fn send_packet(&mut self, id: i32, body: &[u8]) {
        let mut data = Vec::with_capacity(body.len() + 5);
        write_varint(&mut data, id);
        data.extend_from_slice(body);

        let mut frame = Vec::with_capacity(data.len() + 8);
        match self.compression {
            None => {
                write_varint(&mut frame, data.len() as i32);
                frame.extend_from_slice(&data);
            }
            Some(threshold) => {
                let mut inner = Vec::with_capacity(data.len() + 8);
                if data.len() >= threshold as usize {
                    let mut compressed = Vec::new();
                    flate2::write::ZlibEncoder::new(
                        &mut compressed,
                        flate2::Compression::default(),
                    )
                    .write_all(&data)
                    .unwrap();
                    write_varint(&mut inner, data.len() as i32);
                    inner.extend_from_slice(&compressed);
                } else {
                    write_varint(&mut inner, 0);
                    inner.extend_from_slice(&data);
                }
                write_varint(&mut frame, inner.len() as i32);
                frame.extend_from_slice(&inner);
            }
        }

        if let Some(cipher) = &mut self.out {
            cipher.transform(&mut frame);
        }
        self.stream.write_all(&frame).await.unwrap();
        self.stream.flush().await.unwrap();
    }

    async fn handshake(&mut self, next_state: i32) {
        let handshake = Handshake {
            protocol_version: PROTOCOL_VERSION,
            server_address: "localhost".to_string(),
            server_port: 25565,
            next_state,
        };
        let mut body = loadstone_protocol::PacketWriter::new();
        handshake.encode(&mut body);
        self.send_packet(Handshake::ID, body.as_slice()).await;
    }

    async fn login_start(&mut self, name: &str) -> Uuid {
        let uuid = Uuid::new_v4();
        let start = LoginStart {
            name: name.to_string(),
            uuid,
        };
        let mut body = loadstone_protocol::PacketWriter::new();
        start.encode(&mut body);
        self.send_packet(LoginStart::ID, body.as_slice()).await;
        uuid
    }

    async fn acknowledged(&mut self) {
        self.send_packet(LoginAcknowledged::ID, &[]).await;
    }
}

fn read_varint(bytes: &[u8]) -> (u32, usize) {
    let mut value = 0u32;
    let mut position = 0u32;
    for (i, byte) in bytes.iter().enumerate() {
        value |= ((byte & 0x7F) as u32) << position;
        if byte & 0x80 == 0 {
            return (value, i + 1);
        }
        position += 7;
    }
    (value, bytes.len())
}

async fn spawn_server(config: ConnectionConfig) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _result = run_connection(stream, config).await;
    });
    (addr, handle)
}

/// A minimal HTTP server that answers `GET /hasJoined?username=X&serverId=Y`,
/// recording requests so the test can compare `serverId` values.
async fn spawn_mock_sessionserver(
    recorded: Arc<Mutex<Vec<String>>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 512];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 8192 {
            let n = stream.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
        }
        let head = String::from_utf8_lossy(&request);
        let path = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("");
        recorded.lock().unwrap().push(path.to_string());

        // The query is `?username=X&serverId=Y`, so the value ends at `&`.
        let query = path.split('?').nth(1).unwrap_or("");
        let mut username = None;
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("username=") {
                username = Some(value.to_string());
            }
        }
        let body = match username.as_deref() {
            Some("MockPlayer") => {
                format!(r#"{{"id":"{MOCK_PLAYER_ID}","name":"MockPlayer","properties":[]}}"#)
            }
            _ => String::new(),
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    });
    (addr, handle)
}

fn base_config(online_mode: bool) -> ConnectionConfig {
    ConnectionConfig {
        online_mode,
        ..Default::default()
    }
}

/// Answers the server's encryption request the way a vanilla client does: wrap a
/// fresh session key with its RSA public key, echo the verify token, then switch
/// the whole connection to AES/CFB8. Returns the `serverId` hash the client would
/// claim, so the test can compare it with what the server asked Mojang about.
///
/// `token_override` sends something other than the echoed token, which protocol 774
/// allows on the wire and the server must refuse.
async fn answer_encryption_request(client: &mut Client, token_override: Option<Vec<u8>>) -> String {
    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for encryption request");
    assert_eq!(id, EncryptionRequest::ID);
    let request = EncryptionRequest::decode(&mut PacketReader::new(&body)).unwrap();
    assert!(request.server_id.is_empty());
    assert!(request.should_authenticate);
    assert!(!request.public_key.is_empty());
    assert!(!request.verify_token.is_empty());

    let shared_secret: [u8; 16] = rand::random();
    let public_key = RsaPublicKey::from_public_key_der(&request.public_key).unwrap();
    let encrypted_secret = public_key
        .encrypt(&mut rand::thread_rng(), Pkcs1v15Encrypt, &shared_secret)
        .unwrap();

    let response = EncryptionResponse {
        shared_secret: encrypted_secret,
        verify_token: token_override.unwrap_or_else(|| request.verify_token.clone()),
    };
    let mut body = loadstone_protocol::PacketWriter::new();
    response.encode(&mut body);
    client
        .send_packet(EncryptionResponse::ID, body.as_slice())
        .await;

    // From here on the wire is encrypted.
    client.enable_encryption(shared_secret);

    compute_server_id(&shared_secret, &request.public_key)
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_login_reaches_configuration() {
    let (addr, server) = spawn_server(base_config(false)).await;

    let mut client = Client::connect(&addr).await;
    client.handshake(2).await;
    let sent_uuid = client.login_start("Alice").await;

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for set compression");
    assert_eq!(id, SetCompression::ID);
    let mut reader = PacketReader::new(&body);
    let threshold = SetCompression::decode(&mut reader).unwrap();
    assert_eq!(threshold.threshold, loadstone_net::COMPRESSION_THRESHOLD);
    client.compression = Some(threshold.threshold);

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for login success");
    assert_eq!(id, LoginSuccess::ID);
    let success = LoginSuccess::decode(&mut PacketReader::new(&body)).unwrap();
    assert_eq!(success.uuid, sent_uuid);
    assert_eq!(success.username, "Alice");
    assert!(success.properties.is_empty());

    client.acknowledged().await;
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server did not finish login")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn online_login_negotiates_encryption_and_session() {
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (mock_url, mock_server) = spawn_mock_sessionserver(recorded.clone()).await;

    let mut config = base_config(true);
    config.sessionserver_url = format!("http://{mock_url}");
    let (addr, server) = spawn_server(config).await;

    let mut client = Client::connect(&addr).await;
    client.handshake(2).await;
    client.login_start("MockPlayer").await;

    let client_server_id = answer_encryption_request(&mut client, None).await;

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for set compression");
    assert_eq!(id, SetCompression::ID);
    let mut reader = PacketReader::new(&body);
    let threshold = SetCompression::decode(&mut reader).unwrap();
    client.compression = Some(threshold.threshold);

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for login success");
    assert_eq!(id, LoginSuccess::ID);
    let success = LoginSuccess::decode(&mut PacketReader::new(&body)).unwrap();
    assert_eq!(success.uuid.to_string(), MOCK_PLAYER_ID);
    assert_eq!(success.username, "MockPlayer");

    client.acknowledged().await;
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server did not finish login")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), mock_server)
        .await
        .unwrap()
        .unwrap();

    // The serverId the server sent to the session server must equal the one
    // the client computed locally: proves both sides agree on the hash.
    let requests = recorded.lock().unwrap();
    let url = requests.first().expect("sessionserver was not contacted");
    let query_server_id = url
        .split("serverId=")
        .nth(1)
        .expect("missing serverId in query");
    assert_eq!(query_server_id, client_server_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn online_login_rejects_unverified_username() {
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (mock_url, mock_server) = spawn_mock_sessionserver(recorded.clone()).await;

    let mut config = base_config(true);
    config.sessionserver_url = format!("http://{mock_url}");
    let (addr, server) = spawn_server(config).await;
    let mut client = Client::connect(&addr).await;
    client.handshake(2).await;
    client.login_start("Intruder").await;

    answer_encryption_request(&mut client, None).await;

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for login disconnect");
    assert_eq!(id, loadstone_protocol::packets::login::LoginDisconnect::ID);
    let mut reader = PacketReader::new(&body);
    let reason = reader.read_string().unwrap().to_string();
    assert!(
        reason.contains("Failed to verify username"),
        "got: {reason}"
    );

    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), mock_server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn online_login_rejects_an_unpaired_verify_token() {
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (mock_url, mock_server) = spawn_mock_sessionserver(recorded.clone()).await;

    let mut config = base_config(true);
    config.sessionserver_url = format!("http://{mock_url}");
    let (addr, server) = spawn_server(config).await;

    let mut client = Client::connect(&addr).await;
    client.handshake(2).await;
    client.login_start("MockPlayer").await;

    // A well-formed key exchange, but the token from the request is not echoed back.
    answer_encryption_request(&mut client, Some(vec![0u8; 16])).await;

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for login disconnect");
    assert_eq!(id, loadstone_protocol::packets::login::LoginDisconnect::ID);
    let mut reader = PacketReader::new(&body);
    let reason = reader.read_string().unwrap().to_string();
    assert!(reason.contains("Invalid verify token"), "got: {reason}");

    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();

    // The token was refused before any session lookup, so Mojang was never asked and
    // the mock is still waiting for a request that must never arrive.
    assert!(recorded.lock().unwrap().is_empty());
    mock_server.abort();
}
