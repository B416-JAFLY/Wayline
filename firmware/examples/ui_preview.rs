//! Export the exact device renderer without hardware: cargo run --features cardputer-ui --example ui_preview -- <out-dir>
use owl_radio_rust_link::ui::{render, Canvas, Preferences, UiState, HEIGHT, WIDTH};
use owl_radio_rust_link::{feedback::Feedback, ui::render_with_transfers};
use std::{fs, io::Write, path::Path};
use wayline_proto::transfer::{Phase, Reason, Transfer, FRAME_LEN, HEADER_LEN};

const FRAME_MS: u32 = 50;
const SETTLE_MS: u32 = 600;

fn demo_transfer(feedback: &mut Feedback, value: Transfer, now: u32) {
    let mut packet = [0; FRAME_LEN];
    packet[..HEADER_LEN].copy_from_slice(&value.encode());
    // Use the renderer's localized generic peer label, without a local test
    // fixture or a real user's device/file name in the public README.
    assert!(feedback.apply(&packet, now));
}

fn export_readme_demo(out: &Path) -> std::io::Result<()> {
    let out = out.join("readme-demo");
    fs::create_dir_all(&out)?;
    let mut timeline = std::io::BufWriter::new(fs::File::create(out.join("timeline.tsv"))?);
    writeln!(timeline, "file\telapsed_ms\tstage")?;
    let mut canvas = Canvas::new();
    let mut ui = UiState::default();
    let mut feedback = Feedback::new();
    ui.start_welcome(0);
    for frame in 0..53_000 / FRAME_MS {
        let now = frame * FRAME_MS;
        ui.tick(now);
        let keys: &[u8] = match now {
            9000 => b"2",
            12_000 => b"3",
            12_800 => b"B",
            13_600 => b"T",
            14_000 | 14_600 => b"M",
            15_000 => b"A",
            15_500 => b"=",
            16_000 | 17_000 | 18_000 => b"V",
            19_000 => b"4",
            21_000 => b"H",
            23_000 => b"I",
            25_000 => b"R",
            27_000 => b"\x081L", // Cancel reset, return home, switch to Traditional Chinese.
            30_000 | 33_000 => b"L",
            _ => b"",
        };
        for &key in keys {
            ui.key(key, now);
        }
        let stage = match now {
            0..2400 => "Boot logo",
            2400..4000 => "Waiting for PC",
            4000..9000 => "Connected sphere",
            9000..12_000 => "Sharing guide",
            12_000..15_000 => "Preferences",
            15_000..19_000 => "Sounds and volume",
            19_000..21_000 => "More",
            21_000..23_000 => "Connection help",
            23_000..25_000 => "About",
            25_000..27_000 => "Reset confirmation",
            27_000..30_000 => "Traditional Chinese",
            30_000..33_000 => "English",
            33_000..34_000 => "Ready",
            34_000..35_000 => "Send preparation",
            35_000..36_000 => "Send approval",
            36_000..39_000 => "Sending",
            39_000..40_000 => "Remote confirmation",
            40_000..42_000 => "Sent",
            42_000..43_000 => "Duplex preparation",
            43_000..46_000 => "Sending and receiving",
            46_000..47_000 => "Duplex confirmation",
            47_000..49_000 => "Both completed",
            49_000..50_500 => "Receiving",
            _ => "Interrupted",
        };
        if (34_000..42_000).contains(&now) {
            let total = 2 * 1024 * 1024;
            let (phase, done) = match now {
                34_000..35_000 => (Phase::Preparing, 0),
                35_000..36_000 => (Phase::Waiting, 0),
                36_000..39_000 => (Phase::Transferring, total * u64::from(now - 36_000) / 3000),
                39_000..40_000 => (Phase::Confirming, total),
                _ => (Phase::Complete, total),
            };
            demo_transfer(
                &mut feedback,
                Transfer {
                    id: 1,
                    direction: 0,
                    phase,
                    reason: Reason::None,
                    files: 3,
                    done,
                    total,
                    known_total: true,
                },
                now,
            );
        } else if (42_000..49_000).contains(&now) {
            for direction in 0..=1 {
                let total = if direction == 0 {
                    3 * 1024 * 1024
                } else {
                    1024 * 1024
                };
                let (phase, done) = match now {
                    42_000..43_000 if direction == 0 => (Phase::Preparing, 0),
                    42_000..43_000 => (Phase::Waiting, 0),
                    43_000..46_000 => (Phase::Transferring, total * u64::from(now - 43_000) / 3000),
                    46_000..47_000 => (Phase::Confirming, total),
                    _ => (Phase::Complete, total),
                };
                demo_transfer(
                    &mut feedback,
                    Transfer {
                        id: 2 + u64::from(direction),
                        direction,
                        phase,
                        reason: Reason::None,
                        files: 1,
                        done,
                        total,
                        known_total: true,
                    },
                    now,
                );
            }
        } else if now >= 49_000 {
            let total = 1024 * 1024;
            let interrupted = now >= 50_500;
            demo_transfer(
                &mut feedback,
                Transfer {
                    id: 4,
                    direction: 1,
                    phase: if interrupted {
                        Phase::Failed
                    } else {
                        Phase::Transferring
                    },
                    reason: if interrupted {
                        Reason::Interrupted
                    } else {
                        Reason::None
                    },
                    files: 1,
                    done: total * u64::from((now - 49_000).min(1500)) / 3000,
                    total,
                    known_total: true,
                },
                now,
            );
        }
        ui.observe_transfers(&feedback, now);
        render_with_transfers(&mut canvas, &ui, now >= 4000, now, &feedback);
        let filename = format!("{frame:04}.ppm");
        save(&canvas, &out.join(&filename))?;
        writeln!(timeline, "{filename}\t{now}\t{stage}")?;
    }
    Ok(())
}

fn export_airdrop_flow(out: &Path, direction: u8) -> std::io::Result<()> {
    let out = out.join(if direction == 0 {
        "pc-airdrop-flow"
    } else {
        "airdrop-flow"
    });
    fs::create_dir_all(&out)?;
    let mut timeline = std::io::BufWriter::new(fs::File::create(out.join("timeline.tsv"))?);
    writeln!(timeline, "file\telapsed_ms\tstate\tpercent\tdirection")?;
    let mut canvas = Canvas::new();
    let mut ui = UiState::default();
    let mut feedback = Feedback::new();
    render_settled(&mut canvas, &ui, true, 1200, None);
    for frame in 0..24_000 / FRAME_MS {
        let elapsed = frame * FRAME_MS;
        let now = 1200 + elapsed;
        let (state, phase, done) = match (direction, elapsed) {
            (0, 0..2000) => ("home", Phase::Idle, 0),
            (0, 2000..4000) => ("picker", Phase::Idle, 0),
            (0, 4000..7000) => ("discover", Phase::Idle, 0),
            (0, 7000..8000) => ("prepare", Phase::Preparing, 0),
            (_, 0..2000) => ("photos", Phase::Idle, 0),
            (_, 2000..4000) => ("photo", Phase::Idle, 0),
            (_, 4000..6000) => ("share", Phase::Idle, 0),
            (_, 6000..8000) => ("discover", Phase::Idle, 0),
            (_, 8000..12_000) => ("offer", Phase::Waiting, 0),
            (_, 12_000..19_000) => (
                "transfer",
                Phase::Transferring,
                1_048_576 * u64::from(elapsed - 12_000) / 7000,
            ),
            (_, 19_000..20_500) => ("confirm", Phase::Confirming, 1_048_576),
            _ => ("complete", Phase::Complete, 1_048_576),
        };
        if phase != Phase::Idle {
            demo_transfer(
                &mut feedback,
                Transfer {
                    id: 10 + u64::from(direction),
                    direction, // 0: Windows sends; 1: Windows receives. ESP32 bridges both.
                    phase,
                    reason: Reason::None,
                    files: 1,
                    done,
                    total: 1_048_576,
                    known_total: true,
                },
                now,
            );
        }
        ui.observe_transfers(&feedback, now);
        ui.tick(now);
        render_with_transfers(&mut canvas, &ui, true, now, &feedback);
        let filename = format!("{frame:04}.ppm");
        save(&canvas, &out.join(&filename))?;
        writeln!(
            timeline,
            "{filename}\t{elapsed}\t{state}\t{}\t{direction}",
            done * 100 / 1_048_576
        )?;
    }
    Ok(())
}

fn save(canvas: &Canvas, path: &Path) -> std::io::Result<()> {
    let mut file = std::io::BufWriter::new(fs::File::create(path)?);
    write!(file, "P6\n{WIDTH} {HEIGHT}\n255\n")?;
    for &pixel in &canvas.pixels {
        file.write_all(&[
            (((pixel >> 11) & 31) * 255 / 31) as u8,
            (((pixel >> 5) & 63) * 255 / 63) as u8,
            ((pixel & 31) * 255 / 31) as u8,
        ])?;
    }
    Ok(())
}

fn render_settled(
    canvas: &mut Canvas,
    ui: &UiState,
    connected: bool,
    now: u32,
    feedback: Option<&Feedback>,
) {
    // Motion state belongs to the canvas. Give each still its own timeline and
    // advance the same UI/canvas at device cadence before taking the screenshot.
    *canvas = Canvas::new();
    for frame in 0..=SETTLE_MS / FRAME_MS {
        let time = now - SETTLE_MS + frame * FRAME_MS;
        if let Some(feedback) = feedback {
            render_with_transfers(canvas, ui, connected, time, feedback);
        } else {
            render(canvas, ui, connected, time);
        }
    }
}

fn main() -> std::io::Result<()> {
    let out = std::env::args().nth(1).expect("pass an output directory");
    let out = Path::new(&out);
    fs::create_dir_all(out)?;
    let mut canvas = Canvas::new();
    for (language, code) in ["zh-CN", "zh-TW", "en"].iter().enumerate() {
        let mut ui = UiState::new(Preferences::from_word(0x0101_0200 | language as u32));
        for page in 0..4 {
            ui.key(b'1' + page, 0);
            render_settled(&mut canvas, &ui, true, 1200, None);
            save(&canvas, &out.join(format!("{code}-{page}.ppm")))?;
        }
        for (key, name) in [(b'H', "help"), (b'I', "about"), (b'R', "reset")] {
            ui.key(key, 0);
            render_settled(&mut canvas, &ui, true, 1200, None);
            save(&canvas, &out.join(format!("{code}-{name}.ppm")))?;
        }
        ui.key(b'\x08', 0); // Cancel the reset preview.
        ui.key(b'A', 0);
        render_settled(&mut canvas, &ui, true, 1200, None);
        save(&canvas, &out.join(format!("{code}-sound.ppm")))?;
        for name in ["soft", "8bit", "chime", "minimal"] {
            render_settled(&mut canvas, &ui, true, 1200, None);
            save(&canvas, &out.join(format!("{code}-style-{name}.ppm")))?;
            ui.key(b'V', 0);
        }
        ui.key(b'A', 0);
        render_settled(&mut canvas, &ui, true, 1200, None);
        save(&canvas, &out.join(format!("{code}-sound-muted.ppm")))?;
        ui.key(b'A', 0);
        for _ in 0..10 {
            ui.key(b'=', 0);
        }
        render_settled(&mut canvas, &ui, true, 1200, None);
        save(&canvas, &out.join(format!("{code}-sound-max.ppm")))?;
        for _ in 0..20 {
            ui.key(b'-', 0);
        }
        render_settled(&mut canvas, &ui, true, 1200, None);
        save(&canvas, &out.join(format!("{code}-sound-zero.ppm")))?;
        ui.start_welcome(0);
        render_settled(&mut canvas, &ui, false, 1200, None);
        save(&canvas, &out.join(format!("{code}-welcome.ppm")))?;
        for (name, dual, phase) in [
            ("send", false, Phase::Transferring),
            ("duplex", true, Phase::Transferring),
            ("complete", false, Phase::Complete),
            ("interrupted", true, Phase::Failed),
        ] {
            let mut feedback = Feedback::new();
            for direction in 0..=u8::from(dual) {
                let mut packet = [0; FRAME_LEN];
                let value = Transfer {
                    id: 100 + u64::from(direction),
                    direction,
                    phase,
                    reason: if phase == Phase::Failed {
                        Reason::Interrupted
                    } else {
                        Reason::None
                    },
                    files: 1,
                    done: 655360,
                    total: 1048576,
                    known_total: direction == 0,
                };
                packet[..HEADER_LEN].copy_from_slice(&value.encode());
                if let Ok(label) = fs::read(out.join("label.bin")) {
                    if label.len() == FRAME_LEN - HEADER_LEN {
                        packet[HEADER_LEN..].copy_from_slice(&label);
                    }
                }
                assert!(feedback.apply(&packet, 0));
            }
            ui.observe_transfers(&feedback, 0);
            render_settled(&mut canvas, &ui, true, 1200, Some(&feedback));
            save(&canvas, &out.join(format!("{code}-transfer-{name}.ppm")))?;
        }
        // These status-page states are independent of welcome and transfer cards.
        for (name, connected, motion) in [
            ("offline", false, true),
            ("connected", true, true),
            ("motion-off", true, false),
        ] {
            let mut ui = UiState::new(Preferences::from_word(0x0101_0200 | language as u32));
            if !motion {
                ui.key(b'M', 0);
            }
            render_settled(&mut canvas, &ui, connected, 1200, None);
            save(&canvas, &out.join(format!("{code}-orb-{name}.ppm")))?;
        }
    }
    let mut ui = UiState::default();
    render_settled(&mut canvas, &ui, false, 1200, None);
    save(&canvas, &out.join("waiting.ppm"))?;
    // Boot animation and actual page changes, sampled at the firmware's nominal frame cadence.
    canvas = Canvas::new();
    ui.start_welcome(0);
    for frame in 0..110 {
        let now = frame * FRAME_MS;
        ui.tick(now);
        if frame == 64 {
            ui.key(b'4', now);
        }
        if frame == 90 {
            ui.key(b'I', now);
        }
        render(&mut canvas, &ui, true, now);
        save(&canvas, &out.join(format!("motion-{frame:03}.ppm")))?;
    }
    // Ten seconds of the connected status page show rotation and several breaths.
    let mut ui = UiState::default();
    render_settled(&mut canvas, &ui, true, 1200, None);
    for frame in 0..200 {
        let now = 1200 + frame * FRAME_MS;
        ui.tick(now);
        render(&mut canvas, &ui, true, now);
        save(&canvas, &out.join(format!("orb-{frame:03}.ppm")))?;
    }
    // A separate sequence verifies the visible connection and disconnection fades.
    let mut ui = UiState::default();
    render_settled(&mut canvas, &ui, false, 1200, None);
    for frame in 0..160 {
        let elapsed = frame * FRAME_MS;
        let now = 1200 + elapsed;
        let connected = (1000..3500).contains(&elapsed) || elapsed >= 5500;
        ui.tick(now);
        render(&mut canvas, &ui, connected, now);
        save(&canvas, &out.join(format!("orb-transition-{frame:03}.ppm")))?;
    }
    export_readme_demo(out)?;
    export_airdrop_flow(out, 1)?;
    export_airdrop_flow(out, 0)?;
    println!("Exported device renderer frames to {}", out.display());
    Ok(())
}
