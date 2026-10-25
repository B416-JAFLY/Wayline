# Development tasks. `just` lists them. Firmware flashing uses an activated
# ESP-IDF environment and an explicitly supplied serial port.

[private]
default:
    @just --list

# Open the cross-platform build and packaging terminal interface.
tui:
    cargo run --locked -p wayline-tools -- tui

# Build the host crates, including the bridge on Windows.
build-bridge:
    cargo run --locked -p wayline-tools -- host-build

# Test the Rust bridge and pinned AWDL core without hardware.
test-bridge:
    cargo test --locked -p wayline-awdl -p wayline-bridge --all-targets
    cargo test --locked --manifest-path vendor/owly-awdl/Cargo.toml --all-features

# Probe the radio protocol using the Rust serial client.
radio RADIO_PORT COMMAND *ARGS:
    cargo run --locked -p wayline-radio -- {{RADIO_PORT}} {{COMMAND}} {{ARGS}}

# Run the Rust workspace tests on the host
rust-test:
    cargo test --manifest-path Cargo.toml

# Build the host crates available on this operating system.
host-build:
    cargo run --locked -p wayline-tools -- host-build

# Build and test the host crates available on this operating system.
host-test:
    cargo run --locked -p wayline-tools -- host-build --test

# Check ESP-IDF, Python, CMake, Ninja and the Rust esp toolchain without hardware.
firmware-check:
    cargo run --locked -p wayline-tools -- firmware-check

# Build a named board profile using native defaults.
firmware-board BOARD:
    cargo run --locked -p wayline-tools -- firmware-build --board "{{BOARD}}"

# Assemble native host executables, firmware and verified flashing tools.
offline-package BOARD OUTPUT VERSION:
    cargo run --locked -p wayline-tools -- package-build --host-dir target/release --board "{{BOARD}}" --output "{{OUTPUT}}" --version "{{VERSION}}"

# Build an ESP-IDF firmware profile through the Rust tool wrapper.
firmware-build TARGET OUTPUT *ARGS:
    cargo run --locked -p wayline-tools -- firmware-build --target {{TARGET}} --output {{OUTPUT}} {{ARGS}}

# Check the no_std protocol crate and the firmware C ABI shim on the host
rust-check:
    cargo check --manifest-path crates/wayline-proto/Cargo.toml --no-default-features
    cargo check --manifest-path firmware/Cargo.toml

# Build the default Rust Cardputer firmware with ESP-IDF drivers
rust-firmware-build:
    cargo run --locked -p wayline-tools -- firmware-build --target esp32s3 --output firmware/build-rust --sdkconfig-defaults "sdkconfig.defaults;sdkconfig.defaults.cardputer"

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
    cargo run --manifest-path Cargo.toml -p wayline-cli -- receive {{ARGS}}

# Run the Rust radio supervisor in a foreground console on Windows
rust-service *ARGS:
    cargo run --manifest-path Cargo.toml -p wayline-service -- {{ARGS}}

# Build the Rust Windows service (installation is a separate explicit step)
rust-service-build:
    cargo build --manifest-path Cargo.toml -p wayline-service --release

# Build the native floating capsule.
rust-desktop-build:
    cargo build --locked -p wayline-desktop --release

# Run the native capsule for development
rust-desktop:
    cargo run --manifest-path Cargo.toml -p wayline-desktop

# Real process/event/pipe tests with a fake OWL, without opening the radio
rust-service-test:
    cargo test --manifest-path Cargo.toml -p wayline-service --all-targets
