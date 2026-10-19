# Development tasks. `just` lists them. Firmware recipes need ESP-IDF's
# environment (export.ps1) in the shell.
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

port := "COM4"

[private]
default:
    @just --list

# Build the Rust Cardputer Adv firmware in firmware/build-rust
build-firmware:
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-firmware.ps1

# Flash owl-radio over {{port}}
flash: build-firmware
    cd firmware; idf.py -B build-rust -D "SDKCONFIG=build-rust/sdkconfig" -p {{port}} flash

# Build owl-radio for the Cardputer Adv's native USB Serial/JTAG port
build-cardputer: build-firmware

# Flash owl-radio to the Cardputer Adv (replaces its current firmware)
flash-cardputer: flash

# Build owl.exe: OWL for Windows, with the ESP32 as its radio
build-owl:
    ./scripts/build-owl.ps1

# Rebuild patched OWL and test the protocol/Windows queue without hardware
test-owl:
    ./scripts/build-owl.ps1 -Test

# Run OWL on the ESP32 with awdl0 (run from an administrator terminal)
owl *ARGS: build-owl
    ./native/owl/build/owl.exe -i {{port}} -c 6 -N -F -E -S pin -T -v {{ARGS}}

# Probe the radio protocol (hello, capture, ack-test, link)
spike TEST *ARGS:
    python tests/radio_probe.py -p {{port}} {{TEST}} {{ARGS}}

# Run the Rust workspace tests on the host
rust-test:
    cargo test --manifest-path Cargo.toml

# Check the no_std protocol crate and the firmware C ABI shim on the host
rust-check:
    cargo check --manifest-path crates/omdrop-proto/Cargo.toml --no-default-features
    cargo check --manifest-path firmware/Cargo.toml

# Build the Rust Cardputer firmware with ESP-IDF drivers
rust-firmware-build: build-firmware

# Test firmware policy without hardware
rust-firmware-test:
    cargo test --manifest-path firmware/Cargo.toml --all-features
    cargo clippy --manifest-path firmware/Cargo.toml --all-targets --all-features -- -D warnings

# Format the Windows workspace and firmware Rust crates
rust-fmt:
    cargo fmt --manifest-path Cargo.toml --all
    cargo fmt --manifest-path firmware/Cargo.toml

# Run the Rust receiver core against a local IPv6 endpoint
rust-receive *ARGS:
    cargo run --manifest-path Cargo.toml -p omdrop-cli -- receive {{ARGS}}

# Run the Rust radio supervisor in a foreground console on Windows
rust-service *ARGS:
    cargo run --manifest-path Cargo.toml -p omdrop-service -- {{ARGS}}

# Build the Rust Windows service (installation is a separate explicit step)
rust-service-build:
    cargo build --manifest-path Cargo.toml -p omdrop-service --release

# Build the native floating capsule (locates the Windows SDK shader compiler)
rust-desktop-build:
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-desktop.ps1

# Run the native capsule for development
rust-desktop:
    cargo run --manifest-path Cargo.toml -p omdrop-desktop

# Real process/event/pipe tests with a fake OWL, without opening the radio
rust-service-test:
    cargo test --manifest-path Cargo.toml -p omdrop-service --all-targets
