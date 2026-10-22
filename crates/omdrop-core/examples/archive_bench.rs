//! Bounded Owly codec benchmark; does not measure wireless throughput.
use owly_airdrop::proto::archive::compress::{Compressor, Decoder, Encoder};
use std::time::Instant;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut source = vec![0; 64 << 10];
    getrandom::fill(&mut source)?;
    for kind in [Compressor::Gzip, Compressor::DvZip] {
        let start = Instant::now();
        let mut wire = 0usize;
        for _ in 0..256 {
            let mut encoder = Encoder::new(kind, flate2::Compression::fast());
            let mut encoded = encoder.write(&source);
            encoded.extend(encoder.finish());
            wire += encoded.len();
            let mut decoder = Decoder::new(kind, 4 << 20);
            let mut input = &encoded[..];
            let mut output = [0; 64 << 10];
            let mut offset = 0;
            loop {
                let progress = decoder.decode(input, &mut output)?;
                assert_eq!(
                    &output[..progress.produced],
                    &source[offset..offset + progress.produced]
                );
                offset += progress.produced;
                input = &input[progress.consumed..];
                if progress.consumed == 0 && progress.produced == 0 {
                    break;
                }
            }
            assert_eq!(offset, source.len());
            assert!(input.is_empty() && decoder.at_boundary());
        }
        println!(
            "{kind:?}: verified 16 MiB, {wire} encoded bytes in {:?}",
            start.elapsed()
        );
    }
    Ok(())
}
