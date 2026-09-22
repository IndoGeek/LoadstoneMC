//! Prints the light-mask and array counts of captured `map_chunk` bodies.
//!
//! Run with paths to packet bodies:
//! `cargo run -p loadstone-protocol --example chunk_light_report -- a.bin b.bin`

use loadstone_protocol::packets::play::MapChunk;
use loadstone_protocol::{Packet, PacketReader};

fn popcount(words: &[i64]) -> u32 {
    words.iter().map(|word| word.count_ones()).sum()
}

fn report(path: &str) {
    let Ok(body) = std::fs::read(path) else {
        println!("{path}: unreadable");
        return;
    };
    let mut reader = PacketReader::new(&body);
    let chunk = match MapChunk::decode(&mut reader) {
        Ok(chunk) => chunk,
        Err(err) => {
            println!("{path}: decode failed: {err}");
            return;
        }
    };
    println!("--- {path}");
    println!("  chunk ({}, {})", chunk.x, chunk.z);
    println!("  leftover bytes after decode: {}", reader.remaining());
    println!(
        "  sky_light_mask words={} popcount={}",
        chunk.sky_light_mask.len(),
        popcount(&chunk.sky_light_mask)
    );
    println!(
        "  block_light_mask popcount={} empty_sky_mask popcount={} empty_block_mask popcount={}",
        popcount(&chunk.block_light_mask),
        popcount(&chunk.empty_sky_light_mask),
        popcount(&chunk.empty_block_light_mask)
    );
    let sizes: std::collections::BTreeSet<usize> =
        chunk.sky_light.iter().map(|array| array.len()).collect();
    println!(
        "  sky_light arrays={} sizes={sizes:?} block_light arrays={}",
        chunk.sky_light.len(),
        chunk.block_light.len()
    );
    let kinds: Vec<i32> = chunk.heightmaps.iter().map(|(kind, _)| *kind).collect();
    println!("  heightmap kinds={kinds:?}");
    println!("  chunk_data bytes={}", chunk.chunk_data.len());
}

fn main() {
    for path in std::env::args().skip(1) {
        report(&path);
    }
}
