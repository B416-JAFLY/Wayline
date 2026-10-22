# Owly AWDL source

Source: `https://github.com/t4t5/owly`, v0.4.0, commit
`2e9b2b0228432d5e402a9c90a031b419960d0bd1`.
`src/`, `tests/` and `LICENSE` come from that revision; the original crate
description is in `UPSTREAM-README.md`. The standalone manifest retains the
version, Rust requirement and dependencies, and disables publishing.

The Windows bridge needs two times for a received frame: the radio reception
time, corrected for USB latency, and the current host time for scheduling.
`Input::FrameAt` supplies both. `driver::Received::received_at` carries an
optional host `Instant`; absent timestamps retain the upstream behavior.
The driver services due timers before traffic, fairly polls both directions, bounds retry queues,
and joins its worker during drop so callers can safely reclaim the device.
Tests cover these adaptations as well as upstream simulations and vectors.

Keep the MIT license with copied sources and release packages. This crate is
an explicit path dependency and is excluded from the root workspace. Check it
with `cargo test --manifest-path vendor/owly-awdl/Cargo.toml --all-features`.
Its standalone lockfile is tracked for reproducible upstream tests.
Remove the local copy once a pinned upstream release provides these behaviors
and passes the bridge's timestamp, backpressure and shutdown tests.
