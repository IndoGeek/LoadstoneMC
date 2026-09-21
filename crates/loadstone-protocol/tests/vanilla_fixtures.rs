//! Packet layouts checked against packets a real vanilla server actually sent.
//!
//! Every other test in this repository round-trips a packet through this crate's
//! own encoder and decoder. That cannot catch a layout mistake, because both sides
//! are built from the same reading of the protocol: a wrong field width agrees with
//! itself perfectly. Two bugs in this project's history were exactly that shape —
//! AES/CFB8 used the same wrong keystream on both ends, and every clientbound Play
//! id was off by one on both ends.
//!
//! So these fixtures are raw bytes captured from a vanilla 1.21.11 server. The
//! check is that a decode of the real packet **consumes every byte and asserts
//! specific values**: a field that is one byte too wide or too narrow cannot do
//! both.

use loadstone_protocol::chunk::{
    section_index, ChunkSection, Heightmap, HEIGHTMAP_MOTION_BLOCKING, HEIGHTMAP_WORLD_SURFACE,
    OVERWORLD_SECTION_COUNT,
};
use loadstone_protocol::packets::play::{
    ClientPosition, MapChunk, PlayLogin, SetChunkCacheCenter, SpawnPosition,
};
use loadstone_protocol::{Packet, PacketReader};

/// Load a captured packet body from `tests/data/play`.
fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/play")
        .join(format!("{name}.bin"));
    std::fs::read(&path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()))
}

/// Decode a captured body and require that nothing is left over.
///
/// The leftover check is the point: it is what turns "plausible values came out"
/// into "this is the layout of the bytes".
fn decode_exactly<P: Packet>(name: &str) -> P {
    let body = fixture(name);
    let mut reader = PacketReader::new(&body);
    let packet =
        P::decode(&mut reader).unwrap_or_else(|err| panic!("decoding the captured {name}: {err}"));
    assert!(
        reader.is_empty(),
        "the captured {name} is {} bytes long but decoding left some behind, so a \
         field width in this packet is wrong",
        body.len()
    );
    packet
}

/// `x`(26) | `z`(26) | `y`(12), most significant field first, each signed.
///
/// Unpacked here rather than through the encoder's own helper, so the two sides
/// of this check are not the same code.
fn unpack_block_position(packed: i64) -> (i32, i32, i32) {
    let field = |shift: u32, bits: u32| {
        let value = ((packed as u64 >> shift) & ((1 << bits) - 1)) as i32;
        if value >= 1 << (bits - 1) {
            value - (1 << bits)
        } else {
            value
        }
    };
    (field(38, 26), field(0, 12), field(12, 26))
}

#[test]
fn login_play_matches_the_captured_packet_field_for_field() {
    let login: PlayLogin = decode_exactly("login");

    // The join-world set a vanilla server offers, in its order.
    assert_eq!(
        login.world_names,
        vec![
            "minecraft:overworld".to_string(),
            "minecraft:the_nether".to_string(),
            "minecraft:the_end".to_string(),
        ]
    );
    let world = &login.world_state;
    assert_eq!(world.dimension_name, "minecraft:overworld");
    // An index into the captured `minecraft:dimension_type` registry, not a name.
    assert_eq!(world.dimension_id, 0);
    assert!(!login.is_hardcore);
    assert!(login.enable_respawn_screen);
    assert!(!login.do_limited_crafting);
    assert!(!login.enforces_secure_chat);
    assert!(world.is_flat, "the capture ran a superflat world");
    assert!(world.death.is_none());
    // The capture server was in offline mode, with one slot and view-distance 4.
    assert_eq!(login.max_players, 1);
    assert_eq!(login.view_distance, 4);

    // 255 is "no previous game mode" on a first join, and the field is an unsigned
    // byte: an `i8` would call this -1, the same byte with a value that does not
    // mean the same thing.
    assert_eq!(world.previous_gamemode, 255);

    // Negative, and this is the interesting one: the flat capture world's sea
    // level sits *below* the ground at y=-63. A varint read that loses the sign
    // yields 4294967233 instead, which is the sort of number a client renders as
    // an absurd sea level rather than rejecting.
    assert_eq!(world.sea_level, -63);
    assert!(
        world.hashed_seed != 0,
        "the capture server picked a real seed"
    );
}

#[test]
fn client_position_matches_the_captured_packet() {
    let body = fixture("position");
    assert_eq!(
        body.len(),
        61,
        "1 teleport id + 24 position + 24 delta + 8 rotation + 4 flags"
    );

    let packet: ClientPosition = decode_exactly("position");

    // The spawn a superflat world puts a player on: standing on the grass layer.
    assert_eq!(packet.teleport_id, 1);
    assert_eq!(packet.x, 4.5);
    assert_eq!(packet.y, -59.0);
    assert_eq!(packet.z, 7.5);
    assert_eq!(packet.dx, 0.0);
    assert_eq!(packet.dy, 0.0);
    assert_eq!(packet.dz, 0.0);
    assert_eq!(packet.yaw, 0.0);
    assert_eq!(packet.pitch, 0.0);
    // Zero means no field is relative. This is the value for which a varint and an
    // i32 are the same bytes, which is why the width had to be settled by the
    // packet's total length rather than by the value.
    assert_eq!(packet.flags, 0);
}

#[test]
fn spawn_position_matches_the_captured_packet() {
    let packet: SpawnPosition = decode_exactly("spawn_position");
    assert_eq!(packet.dimension_name, "minecraft:overworld");
    assert_eq!(packet.yaw, 0.0);
    assert_eq!(packet.pitch, 0.0);
    // The capture world's spawn is its grass surface: the top block is y=-60 and
    // the position sent is where a player's feet go, one above it.
    assert_eq!(unpack_block_position(packet.position), (0, -59, 0));
}

#[test]
fn set_chunk_cache_center_matches_the_captured_packet() {
    let body = fixture("update_view_position");
    // Two varints, and both fields are small positive numbers in the capture.
    assert_eq!(body.len(), 2);
    let packet: SetChunkCacheCenter = decode_exactly("update_view_position");
    assert!(packet.chunk_x.abs() < 32 && packet.chunk_z.abs() < 32);
}

#[test]
fn map_chunk_matches_the_captured_packet() {
    // The strongest check in this file. A chunk is a nested binary structure with
    // no length prefix per section, so a wrong field width anywhere shifts
    // everything after it. Exact consumption is the first half of the evidence;
    // the geometry below is the second, and it is read with this crate's own
    // chunk decoder rather than with the encoder's.
    let chunk: MapChunk = decode_exactly("map_chunk");

    // A full-height column: 24 sections, packed into one buffer, in order.
    let mut reader = PacketReader::new(&chunk.chunk_data);
    let mut sections = Vec::new();
    while !reader.is_empty() {
        sections.push(ChunkSection::decode(&mut reader).expect("decoding a section"));
    }
    assert_eq!(sections.len(), OVERWORLD_SECTION_COUNT);
    assert!(
        reader.is_empty(),
        "the sections have to add up to exactly the chunk buffer"
    );

    // Both heightmaps claim the same surface, and it is the one the capture world
    // was built with: grass top at y=-60, and the world starts at y=-64, so every
    // column's height above the minimum is 5. The array is 37 longs — the per-long
    // scheme; the spanning one would need 36 — and reading it the other way does not
    // produce 5, which is what makes this the check that settles the packing.
    for kind in [HEIGHTMAP_MOTION_BLOCKING, HEIGHTMAP_WORLD_SURFACE] {
        let (_, packed) = chunk
            .heightmaps
            .iter()
            .find(|(ty, _)| *ty == kind)
            .unwrap_or_else(|| panic!("the capture sent a heightmap of kind {kind}"));
        assert_eq!(packed.len(), 37, "one whole height per long slot");
        let heightmap = Heightmap::from_packed(kind, packed);
        assert!(
            heightmap.values.iter().all(|&value| value == 5),
            "every column should be 5 blocks above the world minimum"
        );
    }

    let bottom = &sections[0];
    // The capture world is bedrock(1) + stone(2) + dirt(1) + grass(1) = 5 layers
    // over a 16×16 footprint, so 5 × 256 blocks are not air.
    assert_eq!(bottom.non_air, 1280);

    // These ids are the capture's own output, recorded in
    // `crates/loadstone-registry/data/block_states.json`. Asserting them is what
    // proves the palette is resolved through the section's palette rather than
    // returned as raw indices — the two differ, and the indices look like
    // perfectly believable block ids.
    let at = |local_y: usize| bottom.blocks.values[section_index(0, local_y, 0)];
    assert_eq!(at(0), 85, "minecraft:bedrock");
    assert_eq!(at(1), 1, "minecraft:stone");
    assert_eq!(at(2), 1, "minecraft:stone");
    assert_eq!(at(3), 10, "minecraft:dirt");
    assert_eq!(at(4), 9, "minecraft:grass_block");
    assert_eq!(at(5), 0, "minecraft:air");

    let non_air = bottom
        .blocks
        .values
        .iter()
        .filter(|&&value| value != 0)
        .count();
    assert_eq!(
        non_air as i16, bottom.non_air,
        "the count has to match the data"
    );
    // Everything above the ground must be empty, or the client renders a solid
    // world.
    assert!(sections[1..].iter().all(|section| section.non_air == 0));

    // Light. The capture sends explicit sky-light arrays, one per section it marks
    // in `sky_light_mask`. Reading the block-light mask as the sky one would give
    // the world no light at all, which is what these two assertions separate.
    assert!(chunk.sky_light_mask.iter().any(|&word| word != 0));
    assert!(chunk.block_light.is_empty());
    for array in &chunk.sky_light {
        assert!(!array.is_empty());
        assert_eq!(
            array.len() % 2048,
            0,
            "a section's light is 4096 nibbles, so an array is a multiple of 2048 bytes"
        );
    }
}

#[test]
fn every_captured_fixture_is_decoded_by_a_test() {
    // A fixture nobody decodes is dead weight, and a new capture would otherwise
    // silently go unverified: the capture tool can add a packet to this directory
    // without this file ever noticing.
    let covered = [
        "login",
        "spawn_position",
        "position",
        "update_view_position",
        "map_chunk",
    ];
    // Captured, but this crate has nothing to decode it with yet: it has no
    // `level_chunks_load_start` packet, because this server does not send one. The
    // bytes are kept for the day it does.
    let waiting_for_a_packet = ["game_state_change"];

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/play");

    for name in covered {
        assert!(
            dir.join(format!("{name}.bin")).exists(),
            "missing {name}.bin"
        );
    }

    let mut uncovered = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".bin") {
            if !covered.contains(&stem) && !waiting_for_a_packet.contains(&stem) {
                uncovered.push(name);
            }
        }
    }
    assert!(
        uncovered.is_empty(),
        "these captured packets have no decode test: {uncovered:?}"
    );
}
