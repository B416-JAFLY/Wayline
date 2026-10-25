# Development guide

[简体中文](DEVELOPMENT.zh-CN.md) · [Project overview](../README.md)

Run commands from the repository root. The Windows Cargo workspace contains `apps/` and
`crates/`; `firmware/` has its own Cargo manifest, lockfile and Xtensa configuration.
For installation and daily use, see the [quick start](QUICKSTART.md).

## Build and test

| Target | Requirements |
| --- | --- |
| Windows apps | Git, Rust MSVC, Visual Studio C++ Build Tools, Windows SDK including `fxc.exe` |
| Firmware | ESP-IDF 6.0 and the ESP32-S3 Xtensa toolchain installed with `espup`; the ESP32-C5 build adds its RISC-V target |

Build and test the portable host applications:

```sh
cargo run --locked -p wayline-tools -- host-build --test
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check -p wayline-proto --no-default-features --locked
```

For a portable host check that does not require Wintun, the Windows SDK, or service installation,
run `cargo run --locked -p wayline-tools -- host-build --test`. On non-Windows
systems it builds and tests the CLI and shared host crates; Windows also includes the
bridge, desktop, and service crates. Pass `--release` to use the release profile.
Serial diagnostics require an explicit `--port` or `WAYLINE_SERIAL_PORT` on every host.

Firmware builds use the Rust wrapper and an activated ESP-IDF environment:

```sh
cargo run --locked -p wayline-tools -- firmware-build --target esp32s3 --output firmware/build-rust --sdkconfig-defaults "sdkconfig.defaults;sdkconfig.defaults.cardputer"
```

The Windows release packaging flow adds the Rust bridge and verified Wintun under `target/release`.

For desktop-only work, use `cargo build --locked -p wayline-desktop` (add `--release` for
release builds). Set `GPUI_FXC_PATH` when the GPUI shader compiler is not on the toolchain path.
The local [stacksafe-macro patch](../vendor/stacksafe-macro/README.md) supports GPUI's
indirect macro dependency; keep its source and license tracked.

Service tests use temporary configuration, a separate pipe and a simulated Rust bridge child:

```sh
cargo test -p wayline-service --all-targets --locked
```

The following ignored test exercises the installed service and leaves it stopped.
Finish transfers before running it:

```sh
cargo test -p wayline-service installed_service_lifecycle_without_administrator_rights -- --ignored
```

Host tests do not replace a firmware build, standard-user installation or device testing.
Run `just --list` for task shortcuts.

### Rust AWDL backend

`apps/bridge` is the supervised wireless child. `crates/wayline-awdl` implements
ESP32 serial I/O, Wintun, IPv6 and board feedback. The AWDL protocol comes from
pinned Owly 0.4.0; [source and adaptation notes](../vendor/owly-awdl/README.md)
record its revision and license. AirDrop uses the pinned owly-airdrop protocol core; wayline-core owns Windows sockets, callbacks, cancellation, identity and board feedback. The firmware wire protocol is unchanged.

```sh
cargo test --locked -p wayline-awdl -p wayline-bridge --all-targets
cargo test --locked --manifest-path vendor/owly-awdl/Cargo.toml --all-features
```

The script writes `wayline-bridge.exe`, verified Wintun 0.14.1, `bridge-licenses`
and `wayline-bridge-build.json` to `target/release` or `target/debug`. It does not
open a port, create an adapter or change services. The service runs only the Rust
bridge. The optional absolute `bridgePath` in `%ProgramData%\wayline\config.json`
overrides `wayline-bridge.exe` beside the service. Installers check that executable
before stopping the service. Builds alone do not update an installed service.
Device interoperability testing is still pending.

Foreground operation requires an administrator terminal, the actual port and a
stopped desktop/service. COM4 below is an example:

```sh
target/release/wayline-bridge.exe --port COM4 --channel 6
```

Foreground operation acquires the device lock; Ctrl+C joins serial and Wintun
workers. `--managed` is reserved for service launches and requires its stop event.
The board continues using `wayline-radio-ui`; feedback failure does not fail transfers.
Session leases and process cleanup retain the service's lifecycle.

```sh
cargo test -p wayline-awdl -p wayline-bridge -p wayline-service -p wayline-core --all-targets --locked
cargo test --manifest-path vendor/owly-airdrop/Cargo.toml --all-features --lib --test vectors --locked
cargo test --manifest-path vendor/owly-awdl/Cargo.toml --all-features --locked
cargo clippy --manifest-path vendor/owly-awdl/Cargo.toml --all-targets --all-features --locked -- -D warnings
```

These host checks use simulated serial/radio endpoints and isolated real pipes and
processes. Actual Wintun operation, ESP32 timing and Apple interoperability require
device validation; host tests do not replace hardware acceptance.

### Firmware build and flashing

```sh
cargo install espup --locked
espup install --targets esp32s3 --std
cargo test --locked --manifest-path firmware/Cargo.toml --all-features
cargo fmt --manifest-path firmware/Cargo.toml -- --check
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
cargo clippy --manifest-path firmware/Cargo.toml --all-targets --all-features --locked -- -D warnings
```

The ESP-IDF build requires an activated ESP-IDF 6.0 environment and writes
`firmware/build-rust/owl-radio.bin`.
Keep configuration in `firmware/sdkconfig.defaults` and `sdkconfig.defaults.cardputer`.

Before flashing, quit desktop and stop any standalone service or monitor holding the port.
In an activated ESP-IDF terminal, replace COM4 with the actual device port:

```sh
$fw = (Resolve-Path firmware).Path
idf.py -C $fw -B "$fw\build-rust" -D "SDKCONFIG=$fw\build-rust\sdkconfig" -p COM4 flash
cargo run --locked -p wayline-radio -- COM4 hello
```

Flashing writes bootloader, partition table and application. If the board remains in download
mode, release G0 and press RST. Reopen desktop afterwards to restore wireless operation.

### Headless ESP32-C5 build

The ESP32-C5 profile is headless: it excludes M5Unified and the Cardputer UI, uses the RISC-V
target and writes to its own build directory.

```sh
espup install --targets esp32s3,esp32c5
idf.py -C firmware -B firmware/build-rust-esp32c5 -D IDF_TARGET=esp32c5 build
idf.py -C firmware -B firmware/build-rust-esp32c5-usb -D IDF_TARGET=esp32c5 build
idf.py -C firmware -B firmware/build-rust-esp32s3-headless -D IDF_TARGET=esp32s3 build
idf.py -C firmware -B firmware/build-rust-esp32s3-headless-usb -D IDF_TARGET=esp32s3 build
```

The default image uses UART0 behind the board's USB-UART bridge at 3 Mbaud; output is
`firmware/build-rust-esp32c5/`. Pass `-C5NativeUsb` when the board wires its native USB
Serial/JTAG controller to the host; that image is written to `firmware/build-rust-esp32c5-usb/`.
Component management stays disabled so the ESP32-S3 `firmware/dependencies.lock` is unchanged.
Headless S3 profiles reuse the 8 MiB Cardputer-compatible partition layout but omit M5Unified
and the Cardputer UI. Both 8 MiB and 16 MiB S3 boards use the 8 MiB image and partition layout;
extra Flash capacity is unused, and 4 MiB boards are rejected. Image headers must match the
manifest capacity. S3 and C5 UART0 and Native USB
profiles can be packaged for Desktop and selected in its firmware page. Build records bind each
profile to its target and sdkconfig hash. For direct ESP-IDF
flashing, use the matching build directory:

```sh
$fw = (Resolve-Path firmware).Path
$out = Join-Path $fw 'build-rust-esp32c5'
idf.py -C $fw -B $out -D IDF_TARGET=esp32c5 -D "SDKCONFIG=$out\sdkconfig" -p COM4 flash
cargo run --locked -p wayline-radio -- COM4 hello
```

The image starts on 2.4 GHz channel 6 and switches to 5 GHz channel 44 or 149 when the host
changes `channel` in `%ProgramData%\wayline\config.json`; restart the service after editing
the file. ESP-IDF 6.0 requires the raw transmit rate before Wi-Fi starts, so the C5 build fixes
HE20/MCS0 and ignores per-frame rate hints. AirDrop interoperability and throughput need
validation on the actual board.

## Desktop

Desktop and CLI share `wayline-core` for transfers, `wayline-host` for preferences, identity and
AWDL selection, `wayline-flash` for firmware operations, and the `wayline-service` pipe client.
The unelevated UI and LocalSystem service use the same `wayline-desktop.exe`; dispatch
`--service` before GPUI, single-instance handling or Explorer registration.

Launch acquires a service session. Tray Quit or `Ctrl+Q` cancels desktop's workers and releases
its sessions; the last on-demand client waits for the Rust bridge reclamation before service shutdown.
Closing the card hides it while work continues. Service registration is currently a manual
Windows operation while the Rust installer migration is in progress.
Launch the UI normally so Explorer drag-and-drop works.

| Option | Behavior |
| --- | --- |
| `--send -- <paths...>` | Open files or forward them to the running instance |
| `--show-in-taskbar` | Show the card in the taskbar and Alt-Tab; retain a usable window if tray registration fails |
| `--unregister-shell` | Remove the current user's Explorer and Send to entries after quitting |
| `--version` | Print the version without GPUI or service initialization |

Device name, receive directory, interface language, notification position and identity live under `%LOCALAPPDATA%\wayline`.
`desktop.json` writes preserve unknown settings and use atomic replacement under a process lock.
Identity creation and advertisement rename history in `receiver-services.json` are also locked.
`download_directory` defaults to Downloads/Wayline; changing it leaves existing files in place
and is blocked during receiving. `language` stores `zh-CN`, `zh-TW`, `en-US`, `ja-JP`, `ko-KR`,
`fr-FR`, `de-DE` or `es-ES`; when absent, Windows UI language selects it and unsupported locales
fall back to English. `desktop_notifications_enabled` controls the separate receive notification
window and defaults to `true`. `notification_position` stores one of six screen positions and
defaults to `top-right`; `notification_duration_seconds` stores an allowed delay of 3, 5, 8, 10,
15 or 30 seconds and defaults to 8. The notification fades in and out. Switching
language updates the window, tray, system dialogs and notifications immediately and re-registers Explorer entries. UI copy lives in
`apps/desktop/locales/*.toml` (eight locales currently). `apps/desktop/build.rs` checks that every
locale has the same keys and placeholders and generates the static copy table at build time.
Adding a language requires a TOML file, a `Language` enum variant and an entry in the build list;
keep every translation complete. Low-level diagnostics from shared modules (transfer, flashing and
local settings validation) remain in their source wording. Full receive errors go to `desktop-receiver.log`.

`pinned` defaults to `false` and keeps the sharing card open on deactivation when enabled.
`drag_activate` and `auto_open_download_dir` default to `true`; they control revealing the
drop target during dragging and opening the first received item's location after confirmation.
Missing or null values use defaults; other non-boolean values report errors. Read failures
are shown on the card, and failed saves leave the previous in-memory value unchanged.

Active send and receive cards show a smoothed speed from progress events; it returns to zero
after one second without progress. Elapsed time starts at the first `Uploading` or `Receiving`
event, including a zero-byte event, and runs through final confirmation. Preparation and waiting
for acceptance are excluded. History average speed divides transfer-stream bytes by this duration.
The sharing card header provides Device firmware and Recent transfers icons.

Pages, lists and long text use a shared scroll area. When content exceeds the viewport, a
persistent proportional scrollbar supports the mouse wheel, thumb dragging and track clicks.
Nested areas scroll the inner content first, then the outer area at its boundary. Bottom
actions remain fixed, including the history deletion confirmation's confirm and cancel buttons.

Up to 200 records live separately in `%LOCALAPPDATA%\wayline\transfer-history.json`, written
atomically under a process lock. The complete history JSON is limited to 4 MiB; the oldest whole
records are evicted to satisfy both limits. A single record that exceeds the 4 MiB limit reports
an explicit error. Success is recorded only after final protocol confirmation.
Records distinguish original file bytes from transfer-stream bytes and retain file names, sent
source paths and successful receive paths, but no file contents. Each recorded file can open its
location; older version-1 records without file details remain readable.
The compact list uses three lines for device/outcome, file overview/size and time/actions.
Details contains transfer counters, elapsed time, average speed, the complete file list,
locations, radio band/channel, transfer mode and failure reasons. Older records may not have
radio metadata.

Deleting a record or clearing history always opens a confirmation with Also delete files and
Remember this option. Both are initially unchecked. Remembering changes only the file-deletion
default, stored as `history_delete_files` in `desktop.json` (absent means `false`); Settings can
change the same default, and confirmation is still required every time. File deletion permanently
removes only explicitly recorded regular files, including sent source files; it never recursively
deletes save directories or follows symbolic links or Windows reparse points. Missing files are
treated as already deleted; other deletion errors keep the affected history available for retry.
Transferred folders retain their top-level names and paths for display and opening their locations.
If any selected record contains a folder, Also delete files rejects the whole batch during
preflight and retains all records, protecting files added to that folder after the transfer.
Open the folder's location to manage its contents, or uncheck Also delete files to delete only history.

Incoming transfers require confirmation. Completion requires archive validation and the final
protocol response. Firmware maintenance pauses the idle receiver and rejects active transfers
or other receivers; cancel/quit waits for the flashing child to exit before releasing the device.

### UI previews and assets

Run the shared renderer without a service, Explorer registration or hardware:

```sh
cargo run -p wayline-desktop --example orb_preview --locked
```

Space or the preview button toggles connection state; Esc closes the preview.
Pass options after `--`: `--borderless` previews the transparent window,
`--notice`, `--long-notice` or `--expanded-notice` previews receive feedback,
and `--long-content` stresses scrolling content. Add `--check-layout` for a one-frame bounds check.

Preview the shared scroll component without hardware:

```sh
cargo run -p wayline-desktop --example scroll_preview --locked
```

The buttons or keys 1/2/3 select long, short and nested content; Esc closes the preview. Use it
to check overflow visibility, wheel scrolling, thumb dragging, track clicks and fixed bottom actions.

`orb_geometry.rs` handles bounded projection and connection transitions; `orb.rs` pauses
animation on hidden pages. `window_frame.rs` preserves per-pixel alpha and excludes transparent
corners from hit testing. Tray state follows bridge readiness independently of card visibility
and is restored after Explorer restarts.

[Branding masters](../assets/branding/README.md) generate checked-in assets with Node.js and
`sharp`; `WAYLINE_SHARP_MODULE` can select an installed package:

```sh
node scripts/assets/generate-desktop-icons.cjs
node scripts/assets/generate-firmware-logo.cjs
```

Normal builds use those assets without regeneration. Desktop embeds its ICO/SVG resources;
the header rasterizes SVG at the display scale, and firmware decodes its bounded RLE logo
directly into the framebuffer.

## CLI and radio diagnostics

Wireless commands acquire an installed `Wayline` session, configure application firewall
rules and select a link-local address on `awdl0`. `status` and `doctor` only read state:
they do not start the service, create identity files or open the serial port.

```sh
cargo run -p wayline-cli -- status --json
cargo run -p wayline-cli -- doctor --json
cargo run -p wayline-cli -- radio status --json
cargo run -p wayline-cli -- radio set --band 5ghz --channel 44 --json
cargo run -p wayline-cli -- discover --timeout 15
cargo run -p wayline-cli -- send --to "iPhone Name" --format auto -- "$env:USERPROFILE\Pictures\photo.jpg"
cargo run -p wayline-cli -- receive
cargo run -p wayline-cli -- receive --accept-all --save "$env:USERPROFILE\Downloads\Wayline" --json
```

Interactive sending can select a recipient by number. Scripts and JSON mode require an exact,
unambiguous `--to`. `--format auto` selects DVZip when advertised, otherwise gzip CPIO.
`--no-ble` disables wake advertisements; `--name` and `--save` override shared settings for
the current command.

`radio status` reads the service-reported board, configured band and channel. `radio set`
acquires a service session and waits for the bridge restart. ESP32-S3 supports only 2.4 GHz
channel 6; ESP32-C5 supports channel 6 and 5 GHz channels 44 and 149. Desktop settings use the
same capability report. The service rejects changes during active sends, receives or firmware
maintenance. Switching restarts AWDL/Wintun; interoperability and throughput still need device
validation. Return to channel 6 before changing the radio port. If an ESP32-S3 is connected
while a 5 GHz channel is saved, select channel 6 in Desktop or run
`radio set --band 2.4ghz --channel 6` to recover.

Discovery retries each mDNS candidate's TCP/TLS `/Discover` probe throughout `--timeout`,
using a backoff capped at three seconds. This allows a peer or its receiver service to become
ready after its initial advertisement.

Receiving shows the sender, files and destination before asking. EOF, cancellation or
`--decision-timeout` (default 60 seconds) denies the request. Noninteractive and JSON receiving
require explicit `--accept-all`, applying only to that invocation.

`Ctrl+C` cancels work, drains receiver workers and sends an mDNS goodbye. Service loss also
cancels active work. One sender and one wireless receiver may run concurrently; a second of
either receives a busy result. Received archives are staged on the destination filesystem
and published only after validation, so failures do not expose partial files.

For interface diagnostics, supply `--link-local ADDRESS --iface-index N` together for the same
live interface. `--bind` instead runs a local TCP receiver without a wireless session or mDNS:

```sh
cargo run -p wayline-cli -- receive --bind "[::1]:8771" --accept-all --save "$env:USERPROFILE\Downloads"
```

Receive progress counts encoded HTTP bytes. Upload success requires the receiver's final
response. Failure diagnostics identify the archive/HTTP stage, byte count, content type and
pending framing operation.

Desktop and wireless CLI enable `ReceiverOptions::allow_dvzip_archive_boundary` for senders
that omit the final zero-length HTTP chunk. This applies only to chunked DVZip: CPIO must
reach its trailer, the current DVZip block must pass checksum/length validation, and the HTTP
chunk must end with CRLF. Buffered or partial TLS/HTTP/DVZip tails still require validation.
At a complete boundary with no pending data, the receiver responds and closes the connection,
emitting `ArchiveBoundary` (`uploadArchiveBoundary` in JSON). Generic core receivers,
`--bind`, Content-Length and gzip uploads require strict HTTP completion.

### JSON and exit codes

`--json` emits JSON Lines with `schemaVersion: 1`, `event` and `data`. Progress and diagnostics
precede exactly one `result` event containing `command`, `ok` and `exitCode`, plus failure
`category` and `message`. Human diagnostics go to stderr; JSON commands never prompt.

```json
{"schemaVersion":1,"event":"result","data":{"command":"discover","ok":true,"exitCode":0}}
```

| Exit code | Meaning |
| --- | --- |
| 0 | Success, including discovery with no matches |
| 2 | Invalid arguments or missing unattended policy |
| 3 | Service, device or requested receiver unavailable |
| 4 | Receiver declined |
| 5 | Operation, connection, file or validation failure |
| 6 | Resource busy |
| 130 | Cancelled |

### Firmware operations

Info validates the package and lists USB candidates without opening ports. Flashing requires
an explicit port and board. Upgrade requires recognizable Wayline firmware and an identical
partition table; reinstall requires both replacement flags:

```sh
wayline-cli firmware info --json
wayline-cli firmware flash --port COM4 --board cardputer-adv
wayline-cli firmware flash --port COM4 --board cardputer-adv --reinstall --yes-replace
wayline-cli firmware flash --port COM5 --board esp32s3-uart0 --reinstall --yes-replace
wayline-cli firmware flash --port COM6 --board esp32s3-native-usb --reinstall --yes-replace
wayline-cli firmware flash --port COM8 --board esp32c5-uart0 --reinstall --yes-replace
wayline-cli firmware flash --port COM8 --board esp32c5-native-usb --reinstall --yes-replace
```

`--manifest PATH` and `--tool PATH` select developer inputs; tool-version validation still
applies. A missing service permits standalone first installation; an installed but unavailable
service is an error. Device locking applies in both modes.

### Serial diagnostics

Quit desktop and stop any standalone service or monitor holding the port. Replace COM4 with
the actual port:

```sh
cargo run --locked -p wayline-radio -- COM4 hello
cargo run --locked -p wayline-radio -- COM4 stats
cargo run --locked -p wayline-radio -- COM4 time
cargo run --locked -p wayline-radio -- COM4 channel 6
cargo run --locked -p wayline-radio -- COM4 filter awdl
```

Protocol framing and codec regression tests run through `cargo test -p wayline-proto` and
`cargo test -p wayline-radio`. Reopen desktop or restart the service afterwards.

## Service configuration and IPC

Configuration is `%ProgramData%\wayline\config.json`; writes are atomic:

```json
{"port":"auto","channel":6,"enabled":true,"bridgePath":null}
```

`port` accepts `auto` or a COM port. Use channel 6 for ESP32-S3; ESP32-C5 can also use AWDL
channels 44 and 149 in the 5 GHz band. `bridgePath` is absolute, or null for `wayline-bridge.exe` beside the service. Logs are `service.log` and `bridge-console.log`.

Desktop uses `--service --on-demand`; service installation and ACL management are currently
manual Windows operations while the Rust installer migration is in progress.

For development, create a separate data directory/config and run an elevated console:

```sh
cargo run -p wayline-service -- --console --data-dir E:\path\to\service-data --pipe wayline-dev
```

Only one supervisor may own a radio. Ctrl+C waits up to five seconds for the Rust bridge shutdown before
terminating and reaping a stuck child; it preserves the configured `enabled` value.

The default control pipe is `\\.\pipe\wayline`. Requests are JSON lines with `cmd`:

| Command | Fields | Effect |
| --- | --- | --- |
| `status` | — | Read radio state |
| `start`, `stop` | — | Persist enabled state and start/stop the Rust bridge |
| `set` | Optional `port`, `channel` | Update radio configuration |
| `firewall` | `exe` | Configure rules for an existing desktop or CLI executable |

Responses contain `state`, `port`, `channel`, `awdl0`, `error` and `enabled`; stop reports
`stopping` until child exit. Local authenticated users may connect. Requests are limited to
8192 bytes with a five-second incomplete-request timeout. Firewall rules allow inbound TCP 8771
and UDP 5353 from `fe80::/10` for desktop/CLI executables.

Use `wayline_service::client::Client` for configurable pipe/timeout and typed errors.
Control `stop` persistently disables wireless; session release and maintenance do not.

The versioned `<control-pipe>.sessions` pipe holds client, send, receive and maintenance leases.
The server validates process IDs, binds tokens to the service instance and reclaims lost clients.
Missing capabilities require a service update; new leases are rejected during final shutdown.

Maintenance rejects active transfers, reaps the Rust bridge and blocks new work without changing `enabled`.
the Rust bridge startup and flashing share a per-port mutex; a Job Object reaps flashing tools before reuse.
Interrupted writes retain readiness failure until a retry passes image verification and HELLO.

Readiness records are `COMx.failed` in the Windows Public known folder's `Wayline/device-state`.
Any outstanding failure blocks `port: "auto"`. Re-enumeration can change the COM number;
a successful retry clears only the new port's record. Explicitly configure the verified new
port with `set`, or inspect the old device before handling its record. Do not clear unrelated
failures. USB identities are not automatically reconciled.

## Offline release packages

Build matching host and firmware outputs from the same checkout, then use new output directories.
Builds and packaging do not start services, open ports or flash hardware.

```sh
cargo run --locked -p wayline-tools -- host-build --release
cargo run --locked -p wayline-tools -- desktop-build --release
cargo run --locked -p wayline-tools -- bridge-build --test
cargo run --locked -p wayline-tools -- firmware-build --target esp32s3 --output firmware/build-rust --sdkconfig-defaults "sdkconfig.defaults;sdkconfig.defaults.cardputer"
cargo run --locked -p wayline-tools -- package-verify path/to/firmware/manifest.json
```

The Rust tool now owns host, desktop, bridge and firmware build orchestration, release-tree
assembly/verification, and Windows service registration. `package-build` creates a staged,
hashed release tree; `release-verify` rejects missing, modified, duplicate, unsafe or unlisted
files. `service-install` updates an existing service transactionally and restores its previous
configuration/state if startup fails; `service-uninstall` stops before removal.

```sh
cargo run --locked -p wayline-tools -- package-build --input target/release --output build/release --version 0.1.0
cargo run --locked -p wayline-tools -- release-verify build/release
cargo run --locked -p wayline-tools -- service-install --binary "C:/Program Files/wayline/wayline-service.exe" --resident
cargo run --locked -p wayline-tools -- service-uninstall
```

Run installer/package checks without modifying the installed service:

```sh
cargo test --locked -p wayline-service -p wayline-core -p wayline-flash
```

Before distribution, test offline installation, first flash, upgrade, interruption recovery and
Apple-device transfers on clean Windows x64 and physical Cardputer Adv hardware.

## Firmware and UI

Rust owns protocol framing, RX/TX, filtering, counters, peer tracking, UI and localization.
`firmware/components/radio/radio_hal.c` wraps ESP-IDF; `radio_ui.cpp` wraps M5Unified display,
keyboard, audio, backlight and NVS. Shared C definitions are
`crates/wayline-proto/include/proto.h`; keep host and firmware layouts aligned.

The firmware is allocation-free. ESP32-S3 builds use the Xtensa toolchain and ESP32-C5 uses the
ESP RISC-V toolchain. Short state accesses use a spinlock; radio configuration and
USB output use separate mutexes. Keep blocking driver calls outside the state spinlock.
UI uses a static 240×135 RGB565 framebuffer; `ui_orb.rs` uses bounded fixed-point geometry.
Animation pauses on hidden pages, transfer cards, sleep or disabled motion.
Board controls are in the [quick start](QUICKSTART.md#board-interface).

### Previews and translations

Export previews through the board's Rust renderer:

```sh
cargo run --manifest-path firmware/Cargo.toml --features cardputer-ui --example ui_preview -- build/cardputer-ui
python scripts/preview/render-ui-preview.py --readme
python scripts/preview/render-airdrop-demo.py --direction iphone-to-pc
python scripts/preview/render-airdrop-demo.py --direction pc-to-iphone
```

`build/cardputer-ui/` contains pages in all three languages, transfer cards, sound settings,
animations and verification records. The Python tools require Pillow and write the README GIFs
to `docs/assets/`. On non-Windows hosts, pass `--font` to the AirDrop compositor to select a CJK font.
These previews do not test wireless or hardware behavior.

After editing `firmware/assets/locales.json`, regenerate both the string table and font subsets
with Pillow, fontTools and the font recorded in `firmware/assets/font-source.txt`:

```sh
python scripts/assets/generate-ui-font.py path/to/NotoSansCJKsc-Regular.otf
cargo fmt --manifest-path firmware/Cargo.toml
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
```

Normal builds use the checked-in generated files and SIL OFL license. Script categories,
dependencies and commands are listed in [`scripts/README.md`](../scripts/README.md).

## Board transfer feedback

Desktop and CLI call `wayline_core::enable_board_feedback()`. `wayline-board` publishes through
the Rust bridge's `wayline-radio-ui` pipe at up to five updates/second with one-second heartbeats.
Feedback failures do not fail file transfers or authorize incoming requests.

Send/receive use separate lanes. New work wakes the display and suspends automatic sleep;
completion cards expire after 10 seconds, failure/cancellation after 30. Eight seconds without
a heartbeat marks active feedback lost. Windows rasterizes Unicode device-name glyphs.

Protocol-v2 `0x08` carries the 1,152-byte frame in `crates/wayline-proto/src/transfer.rs`.
`0x09` queries direction 0/1 and returns a 32-byte header in `0x89`. The local pipe accepts
`0 + frame` or `1 + direction`, returns status 0 plus query data (1 on rejection), then waits
for `0xff`. Publishing confirms the Rust bridge queueing; querying confirms firmware state.

With the Rust bridge and firmware running, test file contents, status lanes, rejection and cancellation:

```sh
cargo run -p wayline-core --example board_loopback
cargo run -p wayline-core --example board_loopback -- --failure-cases
```

## Data path and benchmarks

### Experimental receive aggregation

Opt-in A-MPDU receiving supports Apple → ESP32 → PC. Firmware handles ADDBA/DELBA and hardware
Block Ack; the Rust bridge needs no extra flags. Normal builds disable it; PC → Apple transmit aggregation
is not implemented. Throughput and BA timing require device validation.

```sh
cargo test --locked --manifest-path firmware/Cargo.toml --all-features
```

Outputs and build records use `firmware/build-rust-aggregation/`, with
`sdkconfig.defaults.aggregation`; normal packaging selects `firmware/build-rust/`.
After releasing the serial port, flash from an activated ESP-IDF environment:

```sh
$fw = (Resolve-Path firmware).Path
idf.py -C $fw -B "$fw\build-rust-aggregation" -D "SDKCONFIG=$fw\build-rust-aggregation\sdkconfig" -p COM4 flash
```

Replace COM4 with the actual port, then reopen desktop. Compare the same board, Apple device,
channel, file and placement. Check saved-file hashes, cancellation, repeated transfers,
disconnect/reconnect and loss recovery. Independent captures must confirm ADDBA, aggregated
MPDUs and BA; queueing a response does not prove an over-air agreement.

Release the serial port before querying counters; they persist until reset:

```sh
cargo test --locked -p wayline-proto -p wayline-radio
```

`ampdu_mpdu` counts admitted aggregated MPDUs, `ampdu_rx_drop` counts uplink drops, and
`accepted` counts ADDBA responses queued. `response_errors`, `control_drop` and
`tx_report_drop` expose queue/driver failures. `MSG_STATS` body `BA` returns a 56-byte v1
reply; field definitions are in [proto.h](../crates/wayline-proto/include/proto.h).

The policy permits one peer/TID, 2–16 MPDUs, immediate BA and no A-MSDU inside A-MPDU.
Repeated requests preserve the hardware bitmap; failed setup leaves no new session.
DELBA, inactivity, host loss or radio changes tear it down. Generation checks invalidate
stale requests, and the bounded control/uplink queues keep Wi-Fi callbacks nonblocking.

The C HAL uses receive BA table slot 0 on an unassociated STA; AP/SoftAP is incompatible.
Internal ESP-IDF BA APIs require the SDK archive hash pinned in
[radio CMake configuration](../firmware/components/radio/CMakeLists.txt).
Calls run under the configuration mutex outside the state spinlock. Table readback confirms
programming, not wireless timing; validate on hardware before enabling by default.

### Streaming and codec benchmarks

Firmware uplink batches queued RX and TX-completion messages in a static 4096-byte buffer,
up to eight frames. Each retains its protocol-v2 CRC/COBS delimiter. Ring items are released
before USB writes; at most one pending item remains owned during a flush. Commands use a
separate encoder and lock TX only for complete writes; empty queues never wait to fill a batch.
RX ring capacity is 48 KiB, USB TX/RX buffers are 8 KiB each.

On dual-core native USB builds, driver initialization, its interrupt and uplink run opposite
the Wi-Fi task; driver uninstall must use the install core. UART/single-core builds use normal
initialization. Firmware compiles the shared CRC/COBS codec with `opt-level = 3`.

`MSG_STATS` body `UP` returns fourteen u32le fields (56 bytes, v1): batching, driver bytes,
encoding/write/mutex timings, errors, maxima, heap and interrupt core; see
[proto.h](../crates/wayline-proto/include/proto.h). Totals wrap at u32. These counters measure
uplink traffic; combine them with drops, elapsed transfer time and file hashes.

The shared codec processes segmented inputs in one CRC/COBS pass. the Rust bridge also batches up to
4096 bytes/eight frames, preserving FIFO order and partial-write/backpressure handling.
Receiving streams DVZip into bounded CPIO extraction; sending streams CPIO into gzip/DVZip
temporary archives. Success requires boundary validation and final receiver confirmation.

```sh
cargo run --release -p wayline-proto --example codec_bench
cargo run --release -p wayline-core --example archive_bench
cargo test --manifest-path vendor/owly-airdrop/Cargo.toml --all-features --lib --test vectors --locked
cargo test -p wayline-core --locked
```

Host benchmarks measure codec CPU cost. Measure wireless throughput on hardware with a fixed
board, channel and file, recording elapsed time, integrity and drop counters.
