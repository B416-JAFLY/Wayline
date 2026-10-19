//! Host CPU benchmark. No network, radio, or service changes.
//! cargo run --release -p omdrop-airdrop --example archive_bench
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use omdrop_airdrop::{decode_dvzip, encode_dvzip};
use std::{
    hint::black_box,
    io::{self, Cursor, Read, Write},
    time::Instant,
};

const BLOCK: usize = 64 * 1024;
const STORED: u32 = 0x8000_0000;

// Original implementation retained here solely for reproducible comparisons.
fn old_encode(mut input: impl Read, mut output: impl Write) {
    let mut raw = vec![0; BLOCK];
    loop {
        let mut filled = 0;
        while filled < raw.len() {
            let n = input.read(&mut raw[filled..]).unwrap();
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }
        let mut compressed = Vec::new();
        let mut encoder = ZlibEncoder::new(&mut compressed, Compression::fast());
        encoder.write_all(&raw[..filled]).unwrap();
        encoder.finish().unwrap();
        let stored = compressed.len() >= filled;
        let payload = if stored {
            &raw[..filled]
        } else {
            &compressed[..]
        };
        let header = payload.len() as u32 | if stored { STORED } else { 0 };
        output.write_all(&header.to_be_bytes()).unwrap();
        output.write_all(payload).unwrap();
    }
}

fn old_decode(mut input: impl Read) -> Vec<u8> {
    let mut output = Vec::new();
    let mut header = [0; 4];
    while input.read(&mut header[..1]).unwrap() != 0 {
        input.read_exact(&mut header[1..]).unwrap();
        let word = u32::from_be_bytes(header);
        let mut block = vec![0; (word & !STORED) as usize];
        input.read_exact(&mut block).unwrap();
        if word & STORED != 0 {
            output.extend_from_slice(&block);
        } else {
            io::copy(&mut ZlibDecoder::new(&block[..]), &mut output).unwrap();
        }
    }
    output
}

fn compare(label: &str, bytes: usize, mut old: impl FnMut(), mut new: impl FnMut()) {
    old();
    new();
    let mut before = [0.0; 5];
    let mut after = [0.0; 5];
    for i in 0..5 {
        let start = Instant::now();
        old();
        before[i] = start.elapsed().as_secs_f64();
        let start = Instant::now();
        new();
        after[i] = start.elapsed().as_secs_f64();
    }
    before.sort_by(f64::total_cmp);
    after.sort_by(f64::total_cmp);
    let mib = bytes as f64 / 1_048_576.0;
    println!(
        "{label}: old {:.2} MiB/s, new {:.2} MiB/s, {:.2}x",
        mib / before[2],
        mib / after[2],
        before[2] / after[2]
    );
}

fn main() {
    println!("DVZip host CPU, 8 MiB input, 64 KiB blocks, median of five warm runs");
    for random in [false, true] {
        let mut seed = 42u32;
        let data: Vec<u8> = (0..8 * 1024 * 1024)
            .map(|i| {
                if random {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                } else {
                    b"AirDrop archive payload\n"[i % 24]
                }
            })
            .collect();
        let label = if random { "random" } else { "repeated" };
        let mut old_wire = Vec::new();
        old_encode(Cursor::new(&data), &mut old_wire);
        let mut new_wire = Vec::new();
        let stats = encode_dvzip(Cursor::new(&data), &mut new_wire, BLOCK).unwrap();
        assert_eq!(old_decode(Cursor::new(&new_wire)), data);
        let mut decoded = Vec::new();
        decode_dvzip(Cursor::new(&old_wire), &mut decoded).unwrap();
        assert_eq!(decoded, data);
        drop(decoded);
        println!(
            "{label}: encoded bytes old={}, new={}, blocks={}, stored={}",
            old_wire.len(),
            new_wire.len(),
            stats.blocks,
            stats.stored_blocks
        );
        compare(
            &format!("{label} encode"),
            data.len(),
            || old_encode(Cursor::new(black_box(&data)), io::sink()),
            || {
                black_box(encode_dvzip(Cursor::new(black_box(&data)), io::sink(), BLOCK).unwrap());
            },
        );
        compare(
            &format!("{label} decode"),
            data.len(),
            || {
                black_box(old_decode(Cursor::new(black_box(&old_wire))));
            },
            || {
                black_box(decode_dvzip(Cursor::new(black_box(&old_wire)), io::sink()).unwrap());
            },
        );
    }
}
