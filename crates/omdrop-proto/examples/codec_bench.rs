//! Host CPU microbenchmark, not an ESP32 or over-the-air throughput measurement.
//! cargo run --release -p omdrop-proto --example codec_bench
use omdrop_proto::{cobs_encode, encode_frame_parts, COBS_MAX, RAW_MAX};
use std::{hint::black_box, time::Instant};

// Original firmware pipeline: concatenate, bitwise CRC, then COBS.
fn baseline(parts: &[&[u8]], raw: &mut [u8; RAW_MAX], output: &mut [u8; COBS_MAX]) -> usize {
    let mut len = 0;
    for part in parts {
        raw[len..len + part.len()].copy_from_slice(part);
        len += part.len();
    }
    let mut crc = 0xffffu16;
    for &byte in &raw[..len] {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    raw[len..len + 2].copy_from_slice(&crc.to_le_bytes());
    cobs_encode(&raw[..len + 2], output).unwrap()
}

fn measure(mut f: impl FnMut(), bytes: usize) -> f64 {
    const ITERATIONS: usize = 50_000;
    for _ in 0..1000 {
        f();
    }
    let mut samples = [0.0; 5];
    for sample in &mut samples {
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            f();
        }
        *sample = bytes as f64 * ITERATIONS as f64 / start.elapsed().as_secs_f64() / 1_048_576.0;
    }
    samples.sort_by(f64::total_cmp);
    samples[2]
}

fn main() {
    println!("Host CPU encoding, median of 5 runs, 50000 frames/run (MiB/s)");
    for size in [64, 512, 1600] {
        let payload: Vec<_> = (0..size).map(|i| (i * 131 + i / 7) as u8).collect();
        let parts: &[&[u8]] = &[&[0x90], &[0; 12], &payload];
        let bytes = size + 13;
        let mut raw = [0; RAW_MAX];
        let mut old_output = [0; COBS_MAX];
        let mut new_output = [0; COBS_MAX];
        let old_len = baseline(parts, &mut raw, &mut old_output);
        let new_len = encode_frame_parts(parts, &mut new_output).unwrap();
        assert_eq!(&old_output[..old_len], &new_output[..new_len]);
        let old = measure(
            || {
                let n = baseline(black_box(parts), &mut raw, &mut old_output);
                black_box(&old_output[..n]);
            },
            bytes,
        );
        let new = measure(
            || {
                let n = encode_frame_parts(black_box(parts), &mut new_output).unwrap();
                black_box(&new_output[..n]);
            },
            bytes,
        );
        println!(
            "{size:4} byte payload: before {old:7.2}, after {new:7.2}, speedup {:.2}x",
            new / old
        );
    }
}
