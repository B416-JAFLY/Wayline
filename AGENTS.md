# Project Collaboration Guide

These instructions apply to the entire repository. Communicate in Chinese by default and run
commands from the repository root in PowerShell. Read [README.zh-CN.md](README.zh-CN.md) first;
consult the [development guide](docs/DEVELOPMENT.md) for implementation details.

## Working Practices

- Check Git status and the relevant code before editing. Preserve existing uncommitted changes
  and modify only files involved in the current task.
- Prefer `rg` for code searches. Use call sites, Cargo manifests and build scripts to establish
  dependencies; do not decide that a file is unnecessary based on its name alone.
- Put reusable business logic in the appropriate crate. Application entry points handle UI,
  arguments and workflow coordination.
- Keep changes focused. Do not update unrelated dependencies, reformat unrelated files or
  change the release process as incidental cleanup.
- Document the current structure, behavior and operating procedures. Avoid migration comparisons,
  historical activity logs and duplicated explanations.
- Commit and push according to the user's request. At completion, briefly report the changes,
  actual validation results and anything that remains unverified.

## Module Responsibilities

| Path | Responsibility |
| --- | --- |
| `apps/desktop` | GPUI desktop, tray, Explorer integration, transfer UI and service lifecycle |
| `apps/cli` | AirDrop command-line entry point |
| `crates/omdrop-service` | Windows service, OWL child processes, configuration, named pipes and firewall |
| `crates/omdrop-core` | Discovery, BLE wake-up, TLS, sending and receiving |
| `crates/omdrop-airdrop` | CPIO, gzip, DVZip and plist |
| `crates/omdrop-net` | DNS/mDNS and HTTP codecs |
| `crates/omdrop-proto` | Shared host/firmware CRC/COBS protocol and C protocol header |
| `crates/omdrop-board` | Transfer status feedback to the board |
| `crates/omdrop-radio`, `crates/omdrop-transport` | Serial diagnostics and transport adapters |
| `firmware` | Independent Cargo/ESP-IDF project: ESP32 application, UI, audio and assets |
| `firmware/components/radio` | C/C++ hardware interfaces for ESP-IDF/M5Unified |
| `native/owl` | OWL Windows/USB/Wintun integration and C regression tests |
| `native/owl/patches`, `native/owl/pins` | Upstream patches and pinned revisions |
| `scripts`, `tests` | Build/install scripts and Python diagnostics |

The root Cargo workspace contains Windows applications and shared libraries. `firmware` has
its own `Cargo.toml`, `Cargo.lock` and Xtensa build configuration; build and validate it separately.

## Implementation Constraints

- The desktop UI and privileged service share `omdrop-desktop.exe`, running as a normal user
  process and a separate `--service` process. Dispatch service mode before initializing GPUI,
  single-instance handling or Explorer registration.
- Keep the UI unelevated so Explorer drag-and-drop works. The service manages OWL; only one
  process may own the radio's serial port at a time.
- Closing the sharing card only hides it. Tray Quit must cancel sending, stop receiving and
  wait for the service to reclaim OWL. Preserve error reporting and retry support when shutdown fails.
- `omdrop-service` provides both the library linked into desktop and a standalone service entry
  point. Account for both modes when modifying it.
- Keep `omdrop-proto` usable with `no_std` and the Rust firmware application allocation-free.
  For shared protocol changes, check the Rust definitions, `crates/omdrop-proto/include/proto.h`,
  OWL and firmware together. Verify message lengths, CRC, COBS and boundary handling.
- For FFI changes, check `repr(C)`, field layout, buffer lengths, pointer lifetimes and thread
  access constraints. Keep hardware calls in the C/C++ interface layer. Never make blocking
  driver calls while holding the firmware state spinlock.
- Keep file transfers streaming with bounded buffers. Avoid loading complete files or decoded
  archives into memory. Report success only after final protocol confirmation and data validation;
  cancellation, rejection, disconnection and short reads/writes must also clean up correctly.
- Board status reporting failures must not fail file transfers. Keep display feedback separate
  from user authorization to accept transfers.
- Preserve atomic configuration writes and validate external input boundaries. Include context
  in errors and document the safety conditions of new `unsafe` code.
- The firmware currently targets ESP32-S3/Cardputer Adv, native USB Serial/JTAG and 2.4 GHz
  channel 6. Additional hardware capabilities require implementation and device validation.

## Building and Validation

Start with checks for the affected modules, expanding coverage for shared interfaces or changes
across modules. Tests should cover real behavior and edge cases. Do not add tests for wording,
comments or low-impact mechanical changes. For documentation-only edits, check links, paths
and command syntax.

Windows development requires Rust MSVC, Visual Studio C++ Build Tools and the Windows SDK
(including `fxc.exe`). Build desktop with `scripts/build-desktop.ps1`, which locates the SDK
shader compiler. Add `-DebugBuild` for a debug build.

General Windows workspace checks:

```powershell
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

For protocol or firmware changes, run the relevant checks below. Changes to the firmware
implementation or hardware interfaces also require an actual ESP-IDF firmware build:

```powershell
cargo check -p omdrop-proto --no-default-features --locked
cargo fmt --manifest-path firmware/Cargo.toml -- --check
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
cargo clippy --manifest-path firmware/Cargo.toml --all-targets --all-features --locked -- -D warnings
scripts/build-firmware.ps1
```

Firmware builds require ESP-IDF 6.0 and the ESP32-S3 toolchain installed with `espup`. The script
supports an activated environment, an EIM installation or `-IdfProfile <activation-script>`.
Host tests of firmware logic do not replace an Xtensa build or validation on the board.

For OWL, patch or USB queue changes, put MinGW-w64 GCC and Git on PATH. The build script
discovers installed CMake/Ninja; see the development guide for tool path overrides. Run:

```powershell
scripts/build-owl.ps1 -Test
```

For service changes, start with `cargo test -p omdrop-service --all-targets --locked`. Regular
service tests use temporary configuration, a separate pipe and a simulated OWL child process.
Ignored tests that exercise the installed service change its running state; run them separately
when they fall within the task's scope.

For performance changes, validate correctness, memory bounds and cancellation paths before
running the relevant benchmarks. Codec benchmarks do not replace wireless throughput tests;
see the development guide for benchmark entry points. Investigate test failures rather than
skipping assertions or disabling checks to make them pass.

## Build Files and Assets

- Keep upstream OWL changes in `native/owl/patches`, applied in filename order to the revision
  specified by `native/owl/pins`. The build script reconstructs `.cache/upstream/owl`; do not
  leave changes only in that downloaded checkout.
- Preserve the root and firmware Cargo lockfiles, `firmware/dependencies.lock`, component
  manifests and licenses. Dependency upgrades must relate to the task. Do not remove download
  verification or bypass pinned revisions.
- Put ESP-IDF configuration changes in `firmware/sdkconfig.defaults` or
  `firmware/sdkconfig.defaults.cardputer`. `target`, `firmware/target`, `firmware/build-rust`,
  `native/owl/build`, `.cache`, `firmware/managed_components` and local `build` contents are
  build output or caches and must not be committed.
- Translation source lives in `firmware/assets/locales.json`. When changing text, use
  `scripts/generate-ui-font.py` to regenerate the string table and font subsets together.
  Preserve `firmware/assets/font-source.txt` and `firmware/assets/OFL.txt`; generation steps
  are in the development guide.
- For firmware UI changes, use the `ui_preview` example and `scripts/render-ui-preview.py`
  to check layouts in all three languages, status cards and animation. Display, keyboard,
  audio and persistence across power loss require device testing; previews cannot replace it.
- Keep PowerShell scripts compatible with Windows PowerShell 5.1, check `$LASTEXITCODE` after
  external commands and handle paths containing spaces correctly.

## Deployment and Hardware Operations

Keep building, testing and deployment separate. Install or uninstall services, change firewall
rules, flash firmware and perform real file transfers within the task's authorized scope.
Do not ask again when authorization has already been given. Normal builds must not stop user
services, occupy serial ports or modify devices.

Before flashing or direct serial diagnostics, exit desktop and stop any standalone service or
monitor holding the port. Use the actual device port; COM4 in documentation is an example.
Afterwards, restore the operating state required by the task and report the hardware checks
actually performed.

Update the README and development guide when paths, commands or behavior change. In the
handoff, distinguish host tests, build checks and device tests. Explain why any relevant check
was not run; do not present historical results as validation performed during the current task.
