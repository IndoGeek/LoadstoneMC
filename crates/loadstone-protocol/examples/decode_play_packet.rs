//! Decode a captured clientbound Play packet and print its fields.
//!
//! Used to compare what this crate reads out of a real vanilla packet against
//! what our server writes for the same packet:
//!
//! ```text
//! cargo run -p loadstone-protocol --example decode_play_packet -- login /tmp/x.bin
//! ```

use loadstone_protocol::packets::play::{
    Abilities, ClientPosition, EntityMetadata, Experience, MapChunk, PlayLogin, PlayerInfoUpdate,
    ServerData, SpawnEntity, SyncEntityPosition, UpdateHealth, UpdateTime,
};
use loadstone_protocol::{Packet, PacketReader};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: decode_play_packet <packet> <file>");
        std::process::exit(2);
    }
    let (name, path) = (args[1].as_str(), &args[2]);
    let body = std::fs::read(path)?;

    // Each arm reads the packet and reports how many bytes were left over: a
    // non-zero remainder means the layout in this crate disagrees with the bytes.
    macro_rules! show {
        ($ty:ty) => {{
            let mut reader = PacketReader::new(&body);
            let packet = <$ty>::decode(&mut reader)?;
            println!("{:#?}", packet);
            println!("bytes={} leftover={}", body.len(), reader.remaining());
        }};
    }

    match name {
        "login" => show!(PlayLogin),
        "position" => show!(ClientPosition),
        "map_chunk" => show!(MapChunk),
        "add_entity" => show!(SpawnEntity),
        "set_entity_data" => show!(EntityMetadata),
        "player_info_update" => show!(PlayerInfoUpdate),
        "set_health" => show!(UpdateHealth),
        "set_time" => show!(UpdateTime),
        "set_experience" => show!(Experience),
        "player_abilities" => show!(Abilities),
        "entity_position_sync" => show!(SyncEntityPosition),
        "server_data" => show!(ServerData),
        other => {
            eprintln!("no decoder wired up for {other}");
            std::process::exit(2);
        }
    }
    Ok(())
}
