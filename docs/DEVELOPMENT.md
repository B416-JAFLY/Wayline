# Development guide

[简体中文](DEVELOPMENT.zh-CN.md) · [Project overview](../README.md)

Run PowerShell from the repository root. The Windows Cargo workspace contains `apps/` and
`crates/`; `firmware/` has its own Cargo manifest, lockfile and Xtensa configuration.
For installation and daily use, see the [quick start](QUICKSTART.md).

## Build and test

| Target | Requirements |
| --- | --- |
| Windows apps | Git, Rust MSVC, Visual Studio C++ Build Tools, Windows SDK including `fxc.exe` |
| Firmware | ESP-IDF 6.0 and the ESP32-S3 Xtensa toolchain installed with `espup`; the ESP32-C5 build adds its RISC-V target |

Build the Rust bridge and host applications:

```powershell
scripts/build-bridge.ps1 -Test
scripts/build-release.ps1
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check -p omdrop-proto --no-default-features --locked
```

The default release build includes the Rust bridge and verified Wintun in `target/release`.

For desktop-only work, use `scripts/build-desktop.ps1`, adding `-DebugBuild` for debug.
The script locates the SDK shader compiler; `GPUI_FXC_PATH` overrides it.
The local [stacksafe-macro patch](../vendor/stacksafe-macro/README.md) supports GPUI's
indirect macro dependency; keep its source and license tracked.

Service tests use temporary configuration, a separate pipe and a simulated Rust bridge child:

```powershell
cargo test -p omdrop-service --all-targets --locked
```

The following ignored test exercises the installed service and leaves it stopped.
Finish transfers before running it:

```powershell
cargo test -p omdrop-service installed_service_lifecycle_without_administrator_rights -- --ignored
```

Host tests do not replace a firmware build, standard-user installation or device testing.
Run `just --list` for task shortcuts.

### Rust AWDL backend

`apps/bridge` is the supervised wireless child. `crates/omdrop-awdl` implements
ESP32 serial I/O, Wintun, IPv6 and board feedback. The AWDL protocol comes from
pinned Owly 0.4.0; [source and adaptation notes](../vendor/owly-awdl/README.md)
record its revision and license. AirDrop uses the pinned owly-airdrop protocol core; omdrop-core owns Windows sockets, callbacks, cancellation, identity and board feedback. The firmware wire protocol is unchanged.

```powershell
scripts/build-bridge.ps1 -Test
scripts/build-bridge.ps1 -DebugBuild -Test
```

The script writes `omdrop-bridge.exe`, verified Wintun 0.14.1, `bridge-licenses`
and `omdrop-bridge-build.json` to `target/release` or `target/debug`. It does not
open a port, create an adapter or change services. The service runs only the Rust
bridge. The optional absolute `bridgePath` in `%ProgramData%\omdrop-owl\config.json`
overrides `omdrop-bridge.exe` beside the service. Installers check that executable
before stopping the service. Builds alone do not update an installed service.
Device interoperability testing is still pending.

Foreground operation requires an administrator terminal, the actual port and a
stopped desktop/service. COM4 below is an example:

```powershell
target/release/omdrop-bridge.exe --port COM4 --channel 6
```

Foreground operation acquires the device lock; Ctrl+C joins serial and Wintun
workers. `--managed` is reserved for service launches and requires its stop event.
The board continues using `omdrop-radio-ui`; feedback failure does not fail transfers.
Session leases and process cleanup retain the service's lifecycle.

```powershell
cargo test -p omdrop-awdl -p omdrop-bridge -p omdrop-service -p omdrop-core --all-targets --locked
cargo test --manifest-path vendor/owly-airdrop/Cargo.toml --all-features --lib --test vectors --locked
cargo test --manifest-path vendor/owly-awdl/Cargo.toml --all-features --locked
cargo clippy --manifest-path vendor/owly-awdl/Cargo.toml --all-targets --all-features --locked -- -D warnings
```

These host checks use simulated serial/radio endpoints and isolated real pipes and
processes. Actual Wintun operation, ESP32 timing and Apple interoperability require
device validation; host tests do not replace hardware acceptance.

### Firmware build and flashing

```powershell
cargo install espup --locked
espup install --targets esp32s3 --std
scripts/build-firmware.ps1
cargo fmt --manifest-path firmware/Cargo.toml -- --check
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
cargo clippy --manifest-path firmware/Cargo.toml --all-targets --all-features --locked -- -D warnings
```

The build script accepts an activated ESP-IDF 6.0 environment, discovers an EIM installation,
or takes `-IdfProfile <activation-script>`. Output is `firmware/build-rust/owl-radio.bin`.
Keep configuration in `firmware/sdkconfig.defaults` and `sdkconfig.defaults.cardputer`.

Before flashing, quit desktop and stop any standalone service or monitor holding the port.
In an activated ESP-IDF terminal, replace COM4 with the actual device port:

```powershell
$fw = (Resolve-Path firmware).Path
idf.py -C $fw -B "$fw\build-rust" -D "SDKCONFIG=$fw\build-rust\sdkconfig" -p COM4 flash
python tests/firmware_regression.py -p COM4
```

Flashing writes bootloader, partition table and application. If the board remains in download
mode, release G0 and press RST. Reopen desktop afterwards to restore wireless operation.

### Headless ESP32-C5 build

The ESP32-C5 profile is headless: it excludes M5Unified and the Cardputer UI, uses the RISC-V
target and writes to its own build directory.

```powershell
espup install --targets esp32s3,esp32c5
scripts/build-firmware.ps1 -Target esp32c5
```

The default image uses UART0 behind the board's USB-UART bridge at 3 Mbaud; output is
`firmware/build-rust-esp32c5/`. Pass `-C5NativeUsb` when the board wires its native USB
Serial/JTAG controller to the host; that image is written to `firmware/build-rust-esp32c5-usb/`.
Component management stays disabled so the ESP32-S3 `firmware/dependencies.lock` is unchanged.
The C5 image is not packaged for desktop installation; flash it with ESP-IDF and the matching
build directory:

```powershell
$fw = (Resolve-Path firmware).Path
$out = Join-Path $fw 'build-rust-esp32c5'
idf.py -C $fw -B $out -D IDF_TARGET=esp32c5 -D "SDKCONFIG=$out\sdkconfig" -p COM4 flash
python tests/firmware_regression.py -p COM4 --target esp32c5 --five-ghz
```

The image starts on 2.4 GHz channel 6 and switches to 5 GHz channel 44 or 149 when the host
changes `channel` in `%ProgramData%\omdrop-owl\config.json`; restart the service after editing
the file. ESP-IDF 6.0 requires the raw transmit rate before Wi-Fi starts, so the C5 build fixes
HE20/MCS0 and ignores per-frame rate hints. AirDrop interoperability and throughput need
validation on the actual board.

## Desktop

Desktop and CLI share `omdrop-core` for transfers, `omdrop-host` for preferences, identity and
AWDL selection, `omdrop-flash` for firmware operations, and the `omdrop-service` pipe client.
The unelevated UI and LocalSystem service use the same `omdrop-desktop.exe`; dispatch
`--service` before GPUI, single-instance handling or Explorer registration.

Launch acquires a service session. Tray Quit or `Ctrl+Q` cancels desktop's workers and releases
its sessions; the last on-demand client waits for the Rust bridge reclamation before service shutdown.
Closing the card hides it while work continues. For developer installation, quit the installed
desktop and run `scripts/install-service.ps1` in an administrator terminal; `-Build` builds first.
Launch the UI normally so Explorer drag-and-drop works.

| Option | Behavior |
| --- | --- |
| `--send -- <paths...>` | Open files or forward them to the running instance |
| `--show-in-taskbar` | Show the card in the taskbar and Alt-Tab; retain a usable window if tray registration fails |
| `--unregister-shell` | Remove the current user's Explorer and Send to entries after quitting |
| `--version` | Print the version without GPUI or service initialization |

Device name, receive directory and identity live under `%LOCALAPPDATA%\omdrop`.
`desktop.json` writes preserve unknown settings and use atomic replacement under a process lock.
Identity creation and advertisement rename history in `receiver-services.json` are also locked.
`download_directory` defaults to Downloads/Omdrop; changing it leaves existing files in place
and is blocked during receiving. Full receive errors go to `desktop-receiver.log`.

Incoming transfers require confirmation. Completion requires archive validation and the final
protocol response. Firmware maintenance pauses the idle receiver and rejects active transfers
or other receivers; cancel/quit waits for the flashing child to exit before releasing the device.

### UI previews and assets

Run the shared renderer without a service, Explorer registration or hardware:

```powershell
cargo run -p omdrop-desktop --example orb_preview --locked
```

Space or the preview button toggles connection state; Esc closes the preview.
Pass options after `--`: `--borderless` previews the transparent window,
`--notice`, `--long-notice` or `--expanded-notice` previews receive feedback,
and `--long-content` stresses scrolling content. Add `--check-layout` for a one-frame bounds check.

`orb_geometry.rs` handles bounded projection and connection transitions; `orb.rs` pauses
animation on hidden pages. `window_frame.rs` preserves per-pixel alpha and excludes transparent
corners from hit testing. Tray state follows bridge readiness independently of card visibility
and is restored after Explorer restarts.

[Branding masters](../assets/branding/README.md) generate checked-in assets with Node.js and
`sharp`; `OMDROP_SHARP_MODULE` can select an installed package:

```powershell
node scripts/generate-desktop-icons.cjs
node scripts/generate-firmware-logo.cjs
```

Normal builds use those assets without regeneration. Desktop embeds its ICO/SVG resources;
the header rasterizes SVG at the display scale, and firmware decodes its bounded RLE logo
directly into the framebuffer.

## CLI and radio diagnostics

Wireless commands acquire an installed `OmdropOwl` session, configure application firewall
rules and select a link-local address on `awdl0`. `status` and `doctor` only read state:
they do not start the service, create identity files or open the serial port.

```powershell
cargo run -p omdrop-cli -- status --json
cargo run -p omdrop-cli -- doctor --json
cargo run -p omdrop-cli -- discover --timeout 15
cargo run -p omdrop-cli -- send --to "iPhone Name" --format auto -- "$env:USERPROFILE\Pictures\photo.jpg"
cargo run -p omdrop-cli -- receive
cargo run -p omdrop-cli -- receive --accept-all --save "$env:USERPROFILE\Downloads\Omdrop" --json
```

Interactive sending can select a recipient by number. Scripts and JSON mode require an exact,
unambiguous `--to`. `--format auto` selects DVZip when advertised, otherwise gzip CPIO.
`--no-ble` disables wake advertisements; `--name` and `--save` override shared settings for
the current command.

Receiving shows the sender, files and destination before asking. EOF, cancellation or
`--decision-timeout` (default 60 seconds) denies the request. Noninteractive and JSON receiving
require explicit `--accept-all`, applying only to that invocation.

`Ctrl+C` cancels work, drains receiver workers and sends an mDNS goodbye. Service loss also
cancels active work. One sender and one wireless receiver may run concurrently; a second of
either receives a busy result. Received archives are staged on the destination filesystem
and published only after validation, so failures do not expose partial files.

For interface diagnostics, supply `--link-local ADDRESS --iface-index N` together for the same
live interface. `--bind` instead runs a local TCP receiver without a wireless session or mDNS:

```powershell
cargo run -p omdrop-cli -- receive --bind "[::1]:8771" --accept-all --save "$env:USERPROFILE\Downloads"
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
an explicit port and board. Upgrade requires recognizable Omdrop firmware and an identical
partition table; reinstall requires both replacement flags:

```powershell
omdrop-cli firmware info --json
omdrop-cli firmware flash --port COM4 --board cardputer-adv
omdrop-cli firmware flash --port COM4 --board cardputer-adv --reinstall --yes-replace
```

`--manifest PATH` and `--tool PATH` select developer inputs; tool-version validation still
applies. A missing service permits standalone first installation; an installed but unavailable
service is an error. Device locking applies in both modes.

### Serial diagnostics

Quit desktop and stop any standalone service or monitor holding the port. Replace COM4 with
the actual port; Python diagnostics require `pyserial`:

```powershell
cargo run -p omdrop-radio -- COM4 hello
cargo run -p omdrop-radio -- COM4 stats
python tests/radio_probe.py -p COM4 hello
python tests/firmware_regression.py -p COM4
```

Firmware regression checks commands, corrupt/oversize recovery and repeated TIME requests.
`--tx-frames 20` sends broadcast test frames; `--soak-seconds 20` checks continuous communication.
Reopen desktop or restart the service afterwards.

## Service configuration and IPC

Configuration is `%ProgramData%\omdrop-owl\config.json`; writes are atomic:

```json
{"port":"auto","channel":6,"enabled":true,"bridgePath":null}
```

`port` accepts `auto` or a COM port. Use channel 6 for ESP32-S3; ESP32-C5 can also use AWDL
channels 44 and 149 in the 5 GHz band. `bridgePath` is absolute, or null for `omdrop-bridge.exe` beside the service. Logs are `service.log` and `bridge-console.log`.

Desktop installs `--service --on-demand`. `scripts/install-service.ps1 -Runtime Rust` installs
the standalone service with automatic startup and `--resident`. Both service entry points
accept either lifecycle mode; the default is resident. `-Uninstall` removes service registration
and retains installed files.

For development, create a separate data directory/config and run an elevated console:

```powershell
cargo run -p omdrop-service -- --console --data-dir E:\path\to\service-data --pipe omdrop-dev
```

Only one supervisor may own a radio. Ctrl+C waits up to five seconds for the Rust bridge shutdown before
terminating and reaping a stuck child; it preserves the configured `enabled` value.

The default control pipe is `\\.\pipe\omdrop-owl`. Requests are JSON lines with `cmd`:

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

Use `omdrop_service::client::Client` for configurable pipe/timeout and typed errors.
Control `stop` persistently disables wireless; session release and maintenance do not.

The versioned `<control-pipe>.sessions` pipe holds client, send, receive and maintenance leases.
The server validates process IDs, binds tokens to the service instance and reclaims lost clients.
Missing capabilities require a service update; new leases are rejected during final shutdown.

Maintenance rejects active transfers, reaps the Rust bridge and blocks new work without changing `enabled`.
the Rust bridge startup and flashing share a per-port mutex; a Job Object reaps flashing tools before reuse.
Interrupted writes retain readiness failure until a retry passes image verification and HELLO.

Readiness records are `COMx.failed` in the Windows Public known folder's `Omdrop/device-state`.
Any outstanding failure blocks `port: "auto"`. Re-enumeration can change the COM number;
a successful retry clears only the new port's record. Explicitly configure the verified new
port with `set`, or inspect the old device before handling its record. Do not clear unrelated
failures. USB identities are not automatically reconciled.

## Offline release packages

Build matching host and firmware outputs from the same checkout, then use new output directories.
Builds and packaging do not start services, open ports or flash hardware.

```powershell
scripts/build-release.ps1
scripts/build-firmware.ps1
scripts/package-firmware.ps1 -OutputDirectory build/firmware-package
scripts/fetch-flash-tool.ps1 -OutputDirectory .cache/esptool-5.0.2-release
scripts/package-release.ps1 -OutputDirectory build/release-package -FirmwarePackage build/firmware-package -ToolDirectory .cache/esptool-5.0.2-release/esptool-windows-amd64
scripts/install-prebuilt.ps1 -PackageRoot build/release-package -VerifyOnly
```

Host provenance is `target/release/omdrop-build.json`; firmware provenance is
`firmware/build-rust/omdrop-build.json`. Packaging checks source revisions, versions, hashes,
the pinned Rust bridge build record, image ranges and tool metadata. Firmware `manifest.json` describes the board,
partition layout, protocol and images. `release.json` lists every package file's length and
SHA-256; missing, modified or unlisted files fail verification.

The [download script](../scripts/fetch-flash-tool.ps1) pins and verifies standalone esptool 5.0.2.
Use the tool directory it prints. Flashing validates inputs before maintenance, uses ROM
`--no-stub` commands, verifies each written image, resets and checks HELLO. Upgrade preserves
untouched NVS; an interrupted write may require a full retry.

| Package option | Contents |
| --- | --- |
| Default | Desktop, service, Rust bridge/Wintun, firmware and flashing tool |
| `-IncludeCli` | Add CLI to a runtime package |
| `-Runtime Rust -IncludeCli` | Resident service and CLI, without firmware or flashing tool |
| `-Runtime Cli` | CLI toolkit without installer or service |

A separate CLI toolkit uses an installed service; firmware commands require explicit
`--manifest` and `--tool` paths. Packages include dependency inventories, applicable licenses
and app-local x64 Visual C++ runtime DLLs. The packager discovers the runtime with `vswhere`;
`-VCRuntimeDirectory` and `-VCRuntimeNotice` override its files.

`install.cmd` validates the package before replacing programs under `%ProgramFiles%\omdrop-owl`,
backs up the previous directory and restores files/service configuration on failure.
Quit desktop and CLI first. Run the installer as a normal user so it elevates registration
and launches UI unelevated; an already elevated invocation does not launch UI.
Preferences and downloads remain outside the program directory.
`scripts/install-service.ps1 -PackageRoot <directory>` invokes the same installer.

Run installer/package checks without modifying the installed service:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File tests/release_validation.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File tests/rust_release_validation.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File tests/installer_service_configuration.ps1
```

Before distribution, test offline installation, first flash, upgrade, interruption recovery and
Apple-device transfers on clean Windows x64 and physical Cardputer Adv hardware.

## Firmware and UI

Rust owns protocol framing, RX/TX, filtering, counters, peer tracking, UI and localization.
`firmware/components/radio/radio_hal.c` wraps ESP-IDF; `radio_ui.cpp` wraps M5Unified display,
keyboard, audio, backlight and NVS. Shared C definitions are
`crates/omdrop-proto/include/proto.h`; keep host and firmware layouts aligned.

The firmware is allocation-free. ESP32-S3 builds use the Xtensa toolchain and ESP32-C5 uses the
ESP RISC-V toolchain. Short state accesses use a spinlock; radio configuration and
USB output use separate mutexes. Keep blocking driver calls outside the state spinlock.
UI uses a static 240×135 RGB565 framebuffer; `ui_orb.rs` uses bounded fixed-point geometry.
Animation pauses on hidden pages, transfer cards, sleep or disabled motion.
Board controls are in the [quick start](QUICKSTART.md#board-interface).

### Previews and translations

With Pillow installed, export previews through the board's Rust renderer:

```powershell
cargo run --manifest-path firmware/Cargo.toml --features cardputer-ui --example ui_preview -- build/cardputer-ui
python scripts/render-ui-preview.py
```

`build/cardputer-ui/` contains pages in all three languages, transfer cards, sound settings
and animations. To regenerate the README GIFs from the exported frames:

```powershell
python scripts/render-ui-preview.py --readme
python scripts/render-airdrop-demo.py
python scripts/render-airdrop-demo.py --direction pc-to-iphone
```

Outputs are `docs/assets/cardputer-demo.gif`, `airdrop-esp32-omdrop.gif` and
`omdrop-esp32-airdrop.gif`. The exporters check dimensions, duration and transfer-state ordering;
contact sheets and metadata remain under `build/cardputer-ui/`. Interaction demos use
illustrative phone/desktop panels and actual firmware rendering. The compositor detects a CJK
font or accepts `--font <font-file>`. These previews do not test wireless or hardware behavior.

Edit `firmware/assets/locales.json`, then regenerate strings and font subsets together.
Generation requires Pillow, fontTools and the font named in `firmware/assets/font-source.txt`:

```powershell
python scripts/generate-ui-font.py path/to/NotoSansCJKsc-Regular.otf
cargo fmt --manifest-path firmware/Cargo.toml
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
```

Commit generated subsets and their SIL OFL license; normal builds use the checked-in files.

## Board transfer feedback

Desktop and CLI call `omdrop_core::enable_board_feedback()`. `omdrop-board` publishes through
the Rust bridge's `omdrop-radio-ui` pipe at up to five updates/second with one-second heartbeats.
Feedback failures do not fail file transfers or authorize incoming requests.

Send/receive use separate lanes. New work wakes the display and suspends automatic sleep;
completion cards expire after 10 seconds, failure/cancellation after 30. Eight seconds without
a heartbeat marks active feedback lost. Windows rasterizes Unicode device-name glyphs.

Protocol-v2 `0x08` carries the 1,152-byte frame in `crates/omdrop-proto/src/transfer.rs`.
`0x09` queries direction 0/1 and returns a 32-byte header in `0x89`. The local pipe accepts
`0 + frame` or `1 + direction`, returns status 0 plus query data (1 on rejection), then waits
for `0xff`. Publishing confirms the Rust bridge queueing; querying confirms firmware state.

With the Rust bridge and firmware running, test file contents, status lanes, rejection and cancellation:

```powershell
cargo run -p omdrop-core --example board_loopback
cargo run -p omdrop-core --example board_loopback -- --failure-cases
```

## Data path and benchmarks

### Experimental receive aggregation

Opt-in A-MPDU receiving supports Apple → ESP32 → PC. Firmware handles ADDBA/DELBA and hardware
Block Ack; the Rust bridge needs no extra flags. Normal builds disable it; PC → Apple transmit aggregation
is not implemented. Throughput and BA timing require device validation.

```powershell
scripts/build-firmware.ps1 -RxAggregation
```

Outputs and build records use `firmware/build-rust-aggregation/`, with
`sdkconfig.defaults.aggregation`; normal packaging selects `firmware/build-rust/`.
After releasing the serial port, flash from an activated ESP-IDF environment:

```powershell
$fw = (Resolve-Path firmware).Path
idf.py -C $fw -B "$fw\build-rust-aggregation" -D "SDKCONFIG=$fw\build-rust-aggregation\sdkconfig" -p COM4 flash
```

Replace COM4 with the actual port, then reopen desktop. Compare the same board, Apple device,
channel, file and placement. Check saved-file hashes, cancellation, repeated transfers,
disconnect/reconnect and loss recovery. Independent captures must confirm ADDBA, aggregated
MPDUs and BA; queueing a response does not prove an over-air agreement.

Release the serial port before querying counters; they persist until reset:

```powershell
python tests/radio_probe.py -p COM4 aggregation
python -m unittest discover -s tests -p test_radio_aggregation.py
```

`ampdu_mpdu` counts admitted aggregated MPDUs, `ampdu_rx_drop` counts uplink drops, and
`accepted` counts ADDBA responses queued. `response_errors`, `control_drop` and
`tx_report_drop` expose queue/driver failures. `MSG_STATS` body `BA` returns a 56-byte v1
reply; field definitions are in [proto.h](../crates/omdrop-proto/include/proto.h).

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

After releasing the serial port:

```powershell
python tests/radio_probe.py -p COM4 uplink
```

`MSG_STATS` body `UP` returns fourteen u32le fields (56 bytes, v1): batching, driver bytes,
encoding/write/mutex timings, errors, maxima, heap and interrupt core; see
[proto.h](../crates/omdrop-proto/include/proto.h). Totals wrap at u32. These counters measure
uplink traffic; combine them with drops, elapsed transfer time and file hashes.

The shared codec processes segmented inputs in one CRC/COBS pass. the Rust bridge also batches up to
4096 bytes/eight frames, preserving FIFO order and partial-write/backpressure handling.
Receiving streams DVZip into bounded CPIO extraction; sending streams CPIO into gzip/DVZip
temporary archives. Success requires boundary validation and final receiver confirmation.

```powershell
cargo run --release -p omdrop-proto --example codec_bench
cargo run --release -p omdrop-core --example archive_bench
cargo test --manifest-path vendor/owly-airdrop/Cargo.toml --all-features --lib --test vectors --locked
cargo test -p omdrop-core --locked
```

Host benchmarks measure codec CPU cost. Measure wireless throughput on hardware with a fixed
board, channel and file, recording elapsed time, integrity and drop counters.
