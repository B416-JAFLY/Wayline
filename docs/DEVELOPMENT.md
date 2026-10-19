# Development guide

Run commands from the repository root. The Windows Cargo workspace contains `apps/` and
`crates/`; `firmware/` is an independent ESP32-S3 Cargo/ESP-IDF project. Build prerequisites,
installation and flashing are covered in the [README](../README.md).

The [implementation and acceptance plan (中文)](CLI_DESKTOP_PLAN.zh-CN.md) tracks the shared
CLI/desktop implementation and the remaining device and clean-system acceptance checks.

## Build and test

```powershell
scripts/build-owl.ps1
scripts/build-desktop.ps1
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check -p omdrop-proto --no-default-features --locked
cargo fmt --all -- --check
```

Firmware has its own lockfile and build profile:

```powershell
scripts/build-firmware.ps1
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
cargo clippy --manifest-path firmware/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo fmt --manifest-path firmware/Cargo.toml -- --check
```

OWL requires Git and MinGW-w64 GCC on PATH. `build-owl.ps1` checks prerequisites before fetching
source, and locates CMake/Ninja through PATH, standard CMake/ESP-IDF installations and Visual
Studio's CMake component. ESP-IDF tool lookup also respects `IDF_TOOLS_PATH`. Use `-CMakePath`
and `-NinjaPath` to select executables at custom locations. The script prints the selected paths
and leaves PATH unchanged.

Build the patched OWL source and run its regression tests with the discovered CTest:

```powershell
scripts/build-owl.ps1 -Test
```

These tests run without a physical radio. Service integration tests use a temporary configuration,
a unique named pipe and a simulated OWL child process. `just --list` lists equivalent task shortcuts.

## Desktop

`scripts/build-desktop.ps1 -DebugBuild` creates a debug build. The script discovers the Windows
SDK shader compiler; `GPUI_FXC_PATH` overrides its location. The release executable is
`target/release/omdrop-desktop.exe`.

The sharing homepage renders a depth-shaded point sphere in a fixed-size GPUI canvas.
`orb_geometry.rs` owns the bounded projection and time-based connection transitions;
`orb.rs` paints the points and requests display frames only while the homepage is visible
and the sphere is moving. Bridge readiness drives slow rotation and breathing; disconnecting
smoothly dims and stops the sphere. Hidden pages pause its clock without changing service polling.
To inspect the same renderer without starting a service, registering Explorer integration or
opening a device, run `cargo run -p omdrop-desktop --example orb_preview --locked`.
Click the preview button or press Space to toggle connection state; Esc closes the preview.
Add `-- --borderless` to preview the same transparent window surface used by the application.
`-- --notice`, `-- --long-notice` and `-- --expanded-notice` exercise the shared notice banner,
full-text details and pinned action layout;
the banner's × button dismisses it. Add `--check-layout` to either mode for a one-frame layout
check that measures the actual action, notice and footer bounds and exits without hardware access.
`--long-content --check-layout` stresses the scrolling-content / pinned-action container also
used by the firmware page.
`window_frame.rs` leaves DirectComposition's per-pixel alpha intact so GPUI's rounded edges
can be antialiased, and excludes transparent corners from mouse hit testing. Only the explicit
GPUI fallback with DirectComposition disabled uses a binary window region.

Shared SVG masters and PNG previews live in [`assets/branding`](../assets/branding/README.md).
The desktop logo comes from `assets/branding/omdrop-mark.svg`.
`apps/desktop/assets` contains the square SVG, PNG and multi-size ICO derivatives.
`build.rs` embeds the application and blue/gray tray icons with the Windows SDK resource
compiler through the existing `embed-resource` dependency. The header rasterizes the embedded
SVG with `resvg` at its actual device-pixel size, including fractional display scaling, and
caches up to 16 sizes. This avoids severely shrinking a large bitmap through a texture atlas
without mipmaps. Premultiplied SVG pixels are converted to GPUI's straight-alpha BGRA format
to preserve smooth colored edges. Installed applications do not need loose branding assets.
Normal builds use the
checked-in icon files and do not regenerate them.

To regenerate icons after changing the vector master, use Node.js with `sharp` installed:

```powershell
node scripts/generate-desktop-icons.cjs
```

`OMDROP_SHARP_MODULE` may point to an installed `sharp` package; the generator also detects
the bundled Codex runtime. It writes a light/dark small-size comparison to
`build/desktop-icons-preview.png`. Both tray palettes use identical geometry and alpha.
The tray starts gray and follows `LinkStatus::ready()` from the status poll, independently
of window visibility. Failed shell updates remain pending for retry; after Explorer restarts,
`TaskbarCreated` restores the icon using the latest connection state.

The workspace uses a local [stacksafe-macro patch](../vendor/stacksafe-macro/README.md)
for GPUI's indirect dependency. Its macro diagnostics use `syn::Error`, so desktop
builds do not require `proc-macro-error2`. Keep the patch source and license tracked.

Desktop and CLI link `omdrop-core` for discovery and transfers, `omdrop-host` for preferences,
identity and AWDL selection, and `omdrop-flash` for firmware operations. `omdrop-service` exposes
the common pipe client, SCM access and session leases; desktop also links its `--service`
entry point. The interactive process runs as the user; the LocalSystem service manages OWL,
Wintun and firewall rules. Service mode enters SCM before GPUI or Explorer registration starts.

Launch starts the installed `OmdropOwl` service and acquires a client session. Tray Quit or `Ctrl+Q` cancels
desktop's sending and receiving, waits for its workers, and releases its sessions. Other clients
can keep the service alive; the final on-demand session release waits for OWL to exit before
service shutdown. Closing the sharing card hides it while work continues. Install prebuilt
development binaries with `scripts/install-service.ps1` in an administrator terminal; `-Build`
explicitly builds first. The installer grants interactive users service query/start/stop access.
Launch the UI normally to allow Explorer drag-and-drop.

Useful options:

- `--send -- <paths...>` opens files in the sharing card and forwards to a running instance.
- `--show-in-taskbar` exposes the sharing card in the taskbar and Alt-Tab. If Windows refuses
  tray-icon registration, this explicit mode keeps the window usable and displays a warning;
  `Ctrl+Q` provides Quit. The default tray mode reports the shell-integration error instead
  of hiding an application with no usable tray entry. Keep the UI unelevated in both modes.
- `--unregister-shell` removes the current user's Explorer and Send to entries after quitting.
- `--version` prints the executable version without initializing GPUI or the service.

Device-name and receive-directory preferences are stored in `%LOCALAPPDATA%\omdrop\desktop.json`;
identity files use the same directory. Both frontends share them. Preference writes preserve
unknown settings and use an atomic replacement under a process lock; first-time identity creation
also uses a process lock. CLI `--name` overrides only the current command. The `download_directory` setting selects
the receive destination, defaulting to Downloads/Omdrop when absent; CLI `--save` overrides it
for the current receive command. Desktop Settings → Receive AirDrop → Change save directory
opens the native folder picker. Changing the directory leaves existing files in place and is
unavailable while a receive request or transfer is active.
Settings and Device firmware use centered titles in the compact navigation row, with Back to
sharing on the left; page content scrolls below without a separate large heading.

The desktop accepts incoming transfers only after user confirmation. Its receive card shows
waiting for the sender, transfer progress and final confirmation as separate stages. Known totals
show a percentage bar; unknown totals show received bytes and an activity bar. Completion requires
archive and transfer-boundary validation and a successful final response to the sender.
Receive notices use a bounded single-line banner with copy/dismiss buttons and an eight-second
expiry. Hovering expands the complete text in a scrollable area above the banner and pauses expiry.
The expanded state shows the text once, with only copy/dismiss controls in the bottom row.
A short leave grace allows moving between banner and details; leaving both restarts the eight-second
expiry. Hiding the card clears the hover pause. Copy writes the complete text, including diagnostic
details, and shows feedback for two seconds. Notice IDs keep old hover/copy events from affecting
replacement messages. New receive prompts and file selections clear old notices. Dismissal only
affects presentation, so late confirmed completion can still update the result. Errors initially
show a short recovery hint; full diagnostics are available on hover and remain in
`%LOCALAPPDATA%\omdrop\desktop-receiver.log`. The homepage scrolls its illustration independently
of the file-selection button, keeping the button visible when a notice is present.
`omdrop_host::receiver_advertisement` gives both frontends the same service ID for the same
identity and name. Rename history in `receiver-services.json` is bounded and atomically written
under a process lock so both frontends retire the same previous advertisements.

The Device firmware entry in the sharing-card header uses the bundled `firmware/manifest.json` and
`tools/esptool/esptool.exe`. The user explicitly selects a port and confirms Cardputer Adv.
Firmware information, port selection and installation options are grouped into compact cards.
Upgrade and reinstall are direct choices, with the selected mode's data-preservation warning
beside the board confirmation. Install, cancel and exit-confirmation actions stay below the
scrolling content so long device labels and errors cannot push them out of view.
Desktop pauses its idle receive listener before acquiring maintenance; active transfers and
other clients' receivers prevent flashing. Closing the card keeps flashing active; tray Quit
asks for confirmation before cancelling and waiting for child-process cleanup. A failed or
cancelled write retains the readiness failure until a successful retry completes its boot check.

To exercise the installed service lifecycle, finish transfers and explicitly run:

```powershell
cargo test -p omdrop-service installed_service_lifecycle_without_administrator_rights -- --ignored
```

This test disables administrator-group access for its checks and leaves the service stopped.
It has passed against the installed service. It does not replace testing a standard user session's
UAC installation flow, Explorer drag-and-drop or desktop firmware workflow; the current device
and release acceptance status is recorded in the [implementation plan](CLI_DESKTOP_PLAN.zh-CN.md#实机与发布验收状态).

## CLI and radio diagnostics

Wireless CLI commands start the installed `OmdropOwl` service as needed, acquire the appropriate
session, configure program-specific firewall rules and automatically select a preferred
link-local address on `awdl0`. `status` and `doctor` only inspect state: they do not start the
service, create identity files, modify firewall rules or open the serial port.

```powershell
cargo run -p omdrop-cli -- status --json
cargo run -p omdrop-cli -- doctor --json
cargo run -p omdrop-cli -- discover --timeout 15
cargo run -p omdrop-cli -- send --to "iPhone Name" --format auto -- "$env:USERPROFILE\Pictures\photo.jpg"
cargo run -p omdrop-cli -- receive
cargo run -p omdrop-cli -- receive --accept-all --save "$env:USERPROFILE\Downloads\Omdrop" --json
```

In an interactive terminal, omitting `--to` allows selection by number. Scripts must supply an
exact, unambiguous device name; JSON mode also requires `--to`. Duplicate exact names are an
error. `--format auto` selects DVZip when advertised and gzip cpio otherwise. Upload progress
counts archive bytes; success requires the receiver's final HTTP response. Windows publishes
BLE wake advertisements during discovery and sending; `--no-ble` disables them.

Interactive receiving shows the sender, files and destination before asking for acceptance.
EOF, cancellation or the decision timeout (default 60 seconds, `--decision-timeout`) denies the
request. Noninteractive and JSON receiving require explicit `--accept-all`; this permission
applies only to that invocation. One bounded prompt queue owns terminal input.

`--name NAME` overrides the shared name for one wireless command. For diagnostics,
`--link-local ADDRESS --iface-index N` must be supplied together and match the same live
interface. Explicit `--bind` runs a local TCP receiver without a wireless session or mDNS;
it cannot be combined with those wireless interface parameters:

```powershell
cargo run -p omdrop-cli -- receive --bind "[::1]:8771" --accept-all --save "$env:USERPROFILE\Downloads"
```

`Ctrl+C` cancels discovery, sending or receiving, drains receiver workers, and sends a zero-TTL
mDNS goodbye when using AWDL. Service loss also cancels active work. A second sender or wireless
receiver gets a busy result; one send and one receive may run concurrently. Received archives
are extracted into a temporary directory on the destination filesystem and published only
after archive and transfer-boundary validation; rejected or failed transfers do not publish partial files.

Receive progress counts the encoded HTTP body, so it can differ from the extracted file size.
Upload failures include the archive/HTTP completion stage, received byte count, content type and
pending HTTP framing operation. Socket timeouts such as Windows error 10060 remain connection
failures; malformed chunk boundaries or archive checksums are validation failures.

Desktop and wireless CLI receiving enable `ReceiverOptions::allow_dvzip_archive_boundary` for
AirDrop senders that wait for the response without sending a final zero-length HTTP chunk.
This applies only to chunked DVZip uploads: CPIO must reach its trailer, the current DVZip block
must pass checksum and declared-length validation, and the current HTTP chunk must end with CRLF.
Buffered or partially received TLS, HTTP and DVZip tails still require validation. At a complete
archive boundary with no pending data, the receiver sends its final response and closes the
connection; it does not claim to have received the omitted HTTP terminator. This path emits
`ArchiveBoundary` (`uploadArchiveBoundary` in CLI JSON). Generic core receivers, explicit CLI
`--bind` diagnostics, Content-Length uploads and gzip uploads keep strict HTTP completion.

`--json` writes one object per stdout line with `schemaVersion: 1`, `event` and `data`. Progress
and diagnostic events precede exactly one `result` event; its data contains `command`, `ok` and
`exitCode`, plus `category` and `message` on failure. Human diagnostics use stderr; JSON commands
never ask for terminal input. For example:

```json
{"schemaVersion":1,"event":"result","data":{"command":"discover","ok":true,"exitCode":0}}
```

| Exit code | Meaning |
| --- | --- |
| 0 | Success, including discovery with no matches |
| 2 | Invalid arguments or missing explicit unattended policy |
| 3 | Service, device or requested receiver unavailable |
| 4 | Receiver declined the transfer |
| 5 | Operation, connection, file or validation failure |
| 6 | Resource busy |
| 130 | Cancelled |

Firmware info validates the package and lists candidate USB devices without opening their ports.
Flashing always requires an explicit port and board. The default upgrade requires recognizable
Omdrop firmware and an identical partition table; replacing an existing program requires both
`--reinstall` and `--yes-replace`. Replace COM4 with the actual device port:

```powershell
omdrop-cli firmware info --json
omdrop-cli firmware flash --port COM4 --board cardputer-adv
omdrop-cli firmware flash --port COM4 --board cardputer-adv --reinstall --yes-replace
```

`--manifest PATH` selects an alternate firmware manifest and `--tool PATH` selects a flashing
executable for developer diagnostics. The validated tool version remains fixed. A missing
installed service permits standalone first installation; an installed but unavailable service
is an error, and device-level locking still applies.

Direct serial diagnostics require OWL to be stopped and the serial port released. Replace COM4
with the board's port. Python diagnostics require `pyserial`:

```powershell
cargo run -p omdrop-radio -- COM4 hello
cargo run -p omdrop-radio -- COM4 stats
python tests/radio_probe.py -p COM4 hello
python tests/firmware_regression.py -p COM4
```

Firmware regression covers command responses, corrupt/oversize frame recovery and repeated TIME
requests. Optional `--tx-frames 20` sends broadcast test frames; `--soak-seconds 20` checks
continuous communication. Reopen desktop or start the service after serial diagnostics.

## Service configuration and IPC

Configuration is stored in `%ProgramData%\omdrop-owl\config.json`:

```json
{"port":"auto","channel":6,"enabled":true,"owlPath":null}
```

`port` accepts `auto` or a COM port. Use channel 6 for ESP32-S3. `owlPath` is an absolute path
or null to use `owl.exe` beside the service. Writes are atomic. Logs in the same directory are
`service.log`, `owl.log` and `owl-console.log`.

For headless deployments, `scripts/install-service.ps1 -Runtime Rust` installs the prebuilt standalone
supervisor with automatic startup and `--resident`; client release does not stop this mode.
Desktop installation uses `--service --on-demand`. For development, create a separate data directory and config,
then run an elevated console:

```powershell
cargo run -p omdrop-service -- --console --data-dir E:\path\to\service-data --pipe omdrop-dev
```

Only one supervisor can own a radio. Ctrl+C requests OWL shutdown, waits up to five seconds,
then terminates and reaps a stuck child. Exiting preserves the configured `enabled` value.
Both service entry points accept `--on-demand` or `--resident`; the default is resident.

The default control pipe is `\\.\pipe\omdrop-owl`. Requests are JSON lines with `cmd`:

| Command | Fields | Effect |
| --- | --- | --- |
| `status` | — | Read radio state |
| `start`, `stop` | — | Persist the enabled state and start/stop OWL |
| `set` | Optional `port`, `channel` | Update radio configuration |
| `firewall` | `exe` | Configure rules for an existing desktop or CLI executable |

Responses contain `state`, `port`, `channel`, `awdl0`, `error` and `enabled`. Stop reports
`stopping` until the child exits. The pipe accepts local authenticated users, limits requests to
8192 bytes and expires incomplete requests after five seconds. Firewall rules allow inbound
TCP 8771 and UDP 5353 from `fe80::/10` for `omdrop-desktop.exe` or `omdrop-cli.exe`.

Application code uses `omdrop_service::client::Client` with a configurable pipe name and request
timeout. Its typed errors distinguish connection, timeout, malformed response, service failure,
busy, required update and shutdown. The control `stop` command persistently disables wireless;
it is not a session release or a temporary maintenance operation.

The separate `<control-pipe>.sessions` pipe holds versioned long-lived client, send, receive and
maintenance leases. The server checks the connecting process ID, ties tokens to the service
instance, and reclaims disconnected or exited clients. Missing capabilities require updating the
service. Once final on-demand shutdown starts, new leases are rejected until restart.

Maintenance rejects active send/receive leases, waits for OWL to be reaped, and blocks new radio
work without changing `enabled`. Service OWL startup and flashing share a per-port named mutex.
Flashing uses a Job Object so its tool process is reaped before the device can be reused.
Interrupted writes keep a persistent readiness failure; a retry must pass image verification
and the board HELLO check before its port can resume normal radio operation. See `omdrop-service`
tests for isolated pipe, child-process, session and maintenance cases.

Readiness records are stored as `COMx.failed` under the Windows Public known folder's
`Omdrop/device-state` directory. With `port: "auto"`, any outstanding port failure blocks
automatic OWL startup. USB re-enumeration can change the COM number; a successful retry on a
new port clears only that new port's record and does not establish which device owned the old
record. Automatic discovery therefore remains paused. Explicitly configure the verified new
port through the control pipe's `set` command, or inspect the old device before handling its
record. Do not clear unrelated failures. Cable interruption and cross-port recovery still require
physical-device acceptance; this implementation does not automatically reconcile USB identities.

## Offline release packages

Packaging is a developer operation. Builds and packaging do not start the installed service,
open a serial port or flash a board. Produce matching host and firmware outputs from the same
checkout, then create new output directories:

```powershell
scripts/build-owl.ps1 -Test
scripts/build-release.ps1
scripts/build-firmware.ps1
scripts/package-firmware.ps1 -OutputDirectory build/firmware-package
scripts/fetch-flash-tool.ps1 -OutputDirectory .cache/esptool-5.0.2-release
scripts/package-release.ps1 -OutputDirectory build/release-package -FirmwarePackage build/firmware-package -ToolDirectory .cache/esptool-5.0.2-release/esptool-windows-amd64
scripts/install-prebuilt.ps1 -PackageRoot build/release-package -VerifyOnly
```

`build-release.ps1` records host executable versions, OWL inputs and SHA-256 values in
`target/release/omdrop-build.json`; build OWL before running it. Successful ESP-IDF builds record source revision, image
offsets, lengths and hashes in `firmware/build-rust/omdrop-build.json`. Firmware packaging rejects
changed inputs and collects bootloader, partition table and application from that record.
`manifest.json` declares Cardputer Adv, ESP32-S3, 8 MiB Flash, application/source version,
protocol version 2, ESP-IDF 6.0, partition layout, image hashes and tool version.

The download script pins the Windows x64 standalone esptool **5.0.2** archive to SHA-256
`1caef993a16c5915714a0da772d93f2e3239f316c06223981b262b838287268c`, and retains its executable
layout, license and source archive. Installation and flashing use these local files without downloads.
Use the tool directory printed by the script if its location differs from the example.
`omdrop-flash` validates bounded manifests, safe relative paths, hashes, chip headers, image
and partition ranges before acquiring maintenance. It writes only required images, verifies
them, resets the device and checks HELLO; it does not erase the whole chip. Upgrade compares
the existing partition table before writing and preserves untouched NVS. A verified write and
a ready device are distinct results; cancellation after writing begins can require a full retry.
All device operations use ESP32-S3 ROM commands with `--no-stub`, including chip detection,
partition-table reads, image writes and verification. Each invocation explicitly resets into
ROM download mode. This avoids the pinned esptool stub's large-read stall on the supported
native USB Serial/JTAG connection; verification still checks every written image before the
final reset and HELLO readiness check.

`package-release.ps1` checks host versions and build hashes, recorded OWL binaries/pins/patches,
source revision agreement with the firmware, pinned tool metadata and required license files,
then records every file's length and SHA-256 in `release.json`. Missing, modified or unlisted
files fail package verification. `licenses/dependencies.json` inventories locked Rust dependencies;
their available license texts, ESP-IDF/component licenses, OWL/Wintun notices, font license and
esptool license/source are included with the applicable runtime package.
The default package uses desktop; `-IncludeCli` optionally adds the CLI.
`-Runtime Rust -IncludeCli` creates a headless service package without firmware or flashing tools.
`-Runtime Cli` creates a separate optional CLI toolkit without an installer or service. Run it
alongside an installed desktop or standalone service; supply `--manifest` and `--tool` paths
explicitly when using firmware commands from a separate toolkit directory:

```powershell
scripts/package-release.ps1 -Runtime Cli -OutputDirectory build/cli-package
```

Packages include app-local x64 Visual C++ runtime DLLs and their redistribution notice. The
packager discovers the Visual Studio runtime directory with `vswhere`; `-VCRuntimeDirectory`
selects an explicit `Microsoft.VC*.CRT` directory and `-VCRuntimeNotice` selects its notice file.

Users extract a desktop or headless service package and double-click `install.cmd`. The installer validates files
before stopping the service, stages replacements beside `%ProgramFiles%\omdrop-owl`, retains
the previous directory as a backup, and restores it and the service configuration if installation
fails. It requires desktop and CLI tasks to exit first. Running the entry from a normal account
elevates only installation; starting it from an already elevated terminal does not launch an
elevated UI. Preferences, identity and downloads remain outside the replaced directory.
`scripts/install-service.ps1 -PackageRoot <directory>` forwards to this installer.

Run package boundary, checksum, tampering and service-configuration argument tests without
installing anything. The service-configuration test mocks CIM, checks quoted paths containing
spaces and rollback parameters, and exercises service error propagation without changing SCM:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File tests/release_validation.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File tests/installer_service_configuration.ps1
```

Before distributing a release, complete the clean Windows x64 and physical Cardputer Adv
acceptance list in the [implementation plan](CLI_DESKTOP_PLAN.zh-CN.md), including offline
installation, first flash, upgrade, interruption recovery and Apple-device transfers.

## Firmware and UI

Rust owns protocol framing, command dispatch, RX/TX loops, filtering, counters, peer tracking,
rendering, animation and localization. `firmware/components/radio/radio_hal.c` supplies ESP-IDF
drivers and FreeRTOS resources; `radio_ui.cpp` supplies M5Unified display, keyboard, speaker,
backlight and NVS access. Shared C wire definitions are in `crates/omdrop-proto/include/proto.h`.

The Rust static library is allocation-free and builds `core` with the Xtensa toolchain. Short
state accesses use an SMP critical section; radio configuration and USB output use separate
mutexes. Blocking driver calls run outside the state spinlock.

The UI renders into a static 240×135 RGB565 framebuffer. Preferences are saved to NVS after a
1.2-second debounce. Display sleep leaves radio, keyboard and sound tasks running. Board controls
are listed in the [Chinese README](../README.zh-CN.md#板载界面).

The welcome screen uses the approved full logo from
`assets/branding/omdrop-logo.svg`, with light lettering for the dark panel.
`scripts/generate-firmware-logo.cjs` rasterizes that vector lockup into the checked-in
`firmware/assets/boot-logo.rle` and `firmware/src/ui_brand.rs`. It uses Node.js and `sharp`
with the same `OMDROP_SHARP_MODULE` override as the desktop icon generator. Regenerate with:

```powershell
node scripts/generate-firmware-logo.cjs
```

The 216×52 sprite stores three ink indices and 4-bit coverage in a bounded run-length stream.
Rust decodes directly into the framebuffer without allocation. The two folds settle into
place, the outlined wordmark fades in, and the existing 2.4-second welcome timeout, skip,
replay, localization and reduced-motion controls remain active. Normal builds use the
checked-in sprite; they do not invoke Node.js or rasterize SVGs.

The idle status illustration uses the point-sphere renderer in `firmware/src/ui_orb.rs`.
It uses a constant fixed-point sphere and sine table, a bounded projection buffer, depth shading
and subpixel coverage without heap allocation. The framebuffer and its animation clock remain
zero-initialized in BSS. Status motion follows existing host activity.
Rotation and breathing pause under the welcome screen, other pages, transfer cards, display sleep
or the motion preference. Millisecond wraparound and pause/resume retain the current pose.

Export previews using the same Rust renderer as the board; the Python packager needs Pillow:

```powershell
cargo run --manifest-path firmware/Cargo.toml --features cardputer-ui --example ui_preview -- build/cardputer-ui
python scripts/render-ui-preview.py
```

Outputs in `build/cardputer-ui/` cover main pages, transfer cards, sound settings and animations.
`cardputer-welcome.gif` shows the logo entrance and transition to the status page.
`cardputer-orb.gif` shows the connected status sphere at the device's 20 fps cadence;
`cardputer-orb-transition.gif` shows connection changes, and `cardputer-orb-states.png`
compares offline, connected and motion-disabled states in all three languages.
`ui_preview` also exports a 53-second scripted walkthrough in `build/cardputer-ui/readme-demo/`.
It feeds normal key events and simulated transfer packets through the firmware UI, including
waiting for remote confirmation before completion. No serial port, service or file transfer is used.
To refresh the GIFs referenced by both READMEs after exporting the frames, run:

```powershell
python scripts/render-ui-preview.py --readme
python scripts/render-airdrop-demo.py
python scripts/render-airdrop-demo.py --direction pc-to-iphone
```

The output is `docs/assets/cardputer-demo.gif` at 480×270. A shared palette and coalesced static
frames preserve the 20 fps animation timing and keep the checked-in demonstration compact.
The exporter verifies duration and dimensions and writes visual contact sheets and metadata
under `build/cardputer-ui/readme-demo/`.
The second command creates `docs/assets/airdrop-esp32-omdrop.gif`, a 24-second interaction
walkthrough. The simulated iPhone includes the Photos library, photo viewer, system share
sheet, AirDrop recipient sheet, touch indicators and sending/completion states. The sharing
sequence follows [Apple's AirDrop guide](https://support.apple.com/zh-cn/119857); this is an
illustration, not a recording of a particular iOS release. Its ESP32 panel uses synchronized frames exported to
`build/cardputer-ui/airdrop-flow/`; phone and desktop panels are explicitly illustrative.
No progress is shown before desktop acceptance, and completion follows a separate
confirmation stage. The compositor needs a CJK font: it detects Windows Microsoft YaHei
or Noto/PingFang where installed, or accepts `--font <font-file>`. Verification metadata and
intermediate frames remain under `build/cardputer-ui/airdrop-flow/`.
The third command creates `docs/assets/omdrop-esp32-airdrop.gif`, a separate 24-second
PC-to-iPhone walkthrough using `build/cardputer-ui/pc-airdrop-flow/`. It illustrates the
Windows file picker, Omdrop's device selection and Send action, the iPhone AirDrop acceptance
prompt, upload progress, receiver confirmation and viewing the received photo. The phone
starts with AirDrop reception enabled; acceptance follows the
[iPhone receiving workflow](https://support.apple.com/zh-cn/guide/iphone/iphcd8b9f0af/ios).
Both interaction exports verify the firmware transfer direction, zero progress before acceptance,
100% during confirmation, dimensions and loop duration. UI accent colors are reserved in
the shared GIF palette. These are simulated demonstrations, not wireless/device tests.
Edit translations in `firmware/assets/locales.json`. Font generation needs Pillow, fontTools
and the font specified in `firmware/assets/font-source.txt`:

```powershell
python scripts/generate-ui-font.py path/to/NotoSansCJKsc-Regular.otf
cargo fmt --manifest-path firmware/Cargo.toml
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
```

Generated font subsets and their SIL OFL license are committed; normal builds use these files.

## Board transfer feedback

Desktop and CLI enable `omdrop_core::enable_board_feedback()`. `omdrop-board` publishes status
through OWL's local `omdrop-radio-ui` pipe. Updates are coalesced to five per second with
one-second heartbeats. Feedback failures do not fail transfers.

Send and receive have separate lanes. New work wakes the display and suspends automatic sleep.
Completion cards expire after 10 seconds, failure/cancellation cards after 30. Eight seconds
without a heartbeat marks active feedback as lost. Windows renders Unicode device names into
glyph bitmaps; firmware renders the remaining UI text from its font subsets.

Protocol-v2 message `0x08` carries the 1,152-byte frame defined in
`crates/omdrop-proto/src/transfer.rs`. Message `0x09` queries direction 0/1 and returns its
32-byte header in `0x89`. The local pipe accepts `0 + frame` or `1 + direction`, returns
status 0 plus query data (1 on rejection), then waits for the client's `0xff` acknowledgment.
Publishing confirms that OWL queued a frame; querying confirms firmware state.

With OWL and firmware running, test real board feedback using local IPv6 file transfers:

```powershell
cargo run -p omdrop-core --example board_loopback
cargo run -p omdrop-core --example board_loopback -- --failure-cases
```

These verify file contents, both status lanes, completion, rejection and cancellation.

## Data path and benchmarks

### Experimental receive aggregation

The ESP32-S3 firmware has an opt-in A-MPDU receive implementation for Apple → ESP32 → PC.
It accepts ADDBA and handles DELBA on the board; hardware sends the immediate Block Ack.
OWL needs no new flags or patches. Normal builds leave this disabled. This is an experimental
driver integration: host tests and compilation do not establish over-air operation or a speedup.
Transmit aggregation in the PC → Apple direction is not implemented.

Physical-device reception has confirmed aggregated MPDUs and identical received-file hashes.
Aggregation alone has not demonstrated a throughput gain; bounded USB batching, separate interrupt
affinity and codec optimization have reduced queue drops and improved the measured receive rate.
RX-ring overflow can still occur under bursts; verify speed with the intended board and Apple device.
Independent over-air BA timing, cancellation, disconnection and loss-recovery acceptance remain
required before this integration can be enabled by default.

Build the experimental image separately from the normal firmware:

```powershell
scripts/build-firmware.ps1 -RxAggregation
```

Outputs, sdkconfig and the firmware build record are in `firmware/build-rust-aggregation/`.
The extra defaults are in `firmware/sdkconfig.defaults.aggregation`. The default build still
uses `firmware/build-rust/`; normal packaging does not select the experimental output.
Flashing is a separate operation. After exiting desktop and stopping any standalone service
or monitor owning the actual device port, use an activated IDF environment:

```powershell
$fw = (Resolve-Path firmware).Path
idf.py -C $fw -B "$fw\build-rust-aggregation" -D "SDKCONFIG=$fw\build-rust-aggregation\sdkconfig" -p COM4 flash
```

Replace COM4 with the device's actual port, then reopen desktop. Compare normal and experimental
firmware with the same Apple device, channel, file and placement. Verify saved-file hashes,
cancellation, repeated transfers, disconnect/reconnect and loss recovery as well as throughput.
Use an independent monitor capture to confirm ADDBA success, aggregated MPDUs and hardware BA;
an enqueued ADDBA response alone is not proof that the agreement works on air.

After a transfer, exit desktop/stop the serial owner before querying counters. Reopening the
diagnostic port keeps the board running without asserting DTR/RTS; counters persist until reset:

```powershell
python tests/radio_probe.py -p COM4 aggregation
python -m unittest discover -s tests -p test_radio_aggregation.py
```

`ampdu_mpdu` counts driver-marked aggregated data MPDUs addressed to this radio and admitted
by its filter; `ampdu_rx_drop`
counts their uplink-ring drops. `accepted` counts successful ADDBA response enqueue operations,
not radio acknowledgments. `response_errors`, `control_drop` and `tx_report_drop` expose bounded
queue/driver failures. Querying stats after shutdown can show an inactive session but retains
the counters. `MSG_STATS` with body `BA` returns a 56-byte v1 reply (fourteen u32le fields),
documented in `crates/omdrop-proto/include/proto.h`; older firmware's 28-byte reply is rejected
by the diagnostic decoder. The normal 28-byte statistics and RX framing are unchanged.

The allocation-free Rust policy admits one peer/TID, a receive window of 2–16 MPDUs, immediate
BA and no A-MSDU inside A-MPDU. Repeated requests resend the response without rewinding the
hardware bitmap. Invalid/unsupported requests are ignored or declined; table installation or
response enqueue failure does not leave a new active session. Matching DELBA, inactivity,
host loss and radio reconfiguration tear it down. Generation checks invalidate queued requests
across MAC/channel/filter resets. The eight-entry control queue never blocks Wi-Fi callbacks.
TX completion reports also use the bounded uplink ring, so USB backpressure cannot block the
Wi-Fi callback; counters remain available even if completion telemetry is dropped.

The C HAL uses hardware receive BA table slot 0 while the radio is an unassociated STA; AP
association/SoftAP use is incompatible. `hal_agreement_add_rx_ba(index, ifx, peer, tid, ssn, window)`
and `hal_agreement_del_rx_ba(index)` are internal ESP-IDF symbols. Their scalar/pointer ABI and
slot-0 readback layout were checked against ESP32-S3 ESP-IDF 6.0 `libpp.a` / `hal_ampdu.o`.
CMake requires SHA-256 `47da9a1daf730f5d4867884c9ef6bf5e57ff67265fe1d18c9ccc9d0f3a22b6fd`
when enabled; unknown SDK archives fail configuration. No SDK binary is patched. The readback
check confirms table programming, not wireless timing. All calls stay in the C hardware layer
under the configuration mutex, outside the firmware state spinlock. Device validation is required
before enabling this integration by default or distributing it as a supported feature.

### Streaming and codec benchmarks

Firmware uplink drains already queued RX and TX-completion messages into a static 4096-byte
batch, capped at eight frames. Every message retains its own protocol-v2 CRC/COBS delimiter.
Encoded RX ring items are returned before waiting for the USB writer; at most one pending item
can remain owned while a full batch is flushed. Commands use a
separate encoder and take the shared TX mutex only across their complete writes. There is no
wait to fill a batch when the queue is empty. The RX ring remains 48 KiB and USB TX/RX buffers
remain 8 KiB each; the batch adds 4 KiB of static memory rather than enlarging the queues.
On dual-core native USB builds, a short initialization task installs the USB Serial/JTAG driver
on the core opposite the configured Wi-Fi task, and uplink stays on that core. This also places
the USB interrupt there, as required by the ESP-IDF driver contract. UART/single-core builds
retain the normal initialization path. Any future USB driver uninstall must use its install core.
The firmware's release profile keeps the application size-optimized but compiles the shared
`omdrop-proto` CRC/COBS implementation with `opt-level = 3`; host build profiles are unchanged.

After releasing the serial port, inspect the new uplink diagnostics:

```powershell
python tests/radio_probe.py -p COM4 uplink
```

`MSG_STATS` with body `UP` returns fourteen u32le fields (56 bytes): version 1, frames, batches,
driver-accepted bytes, encoding microseconds, driver-write microseconds, maximum write duration,
TX-mutex wait microseconds, encoding/write errors, maximum batch frames/bytes, free/minimum internal
heap and the link interrupt core. Totals wrap at u32 (microsecond totals after about 71 minutes).
These counters cover queued uplink traffic, not command responses or final file delivery. Combine
them with RX-drop/aggregation counters, elapsed file-transfer time and file hashes when evaluating
performance. Older firmware's ordinary 28-byte stats reply is rejected by the diagnostic decoder.

`omdrop-proto` encodes segmented inputs in one CRC/COBS pass. Firmware uses the same codec
without allocation. The OWL USB writer batches queued frames up to 4096 bytes and eight frames,
preserving FIFO order and handling partial/overlapped writes and queue backpressure.

Receiving streams DVZip through a bounded decoder into CPIO extraction and validates trailing
blocks before reporting success. Sending streams CPIO into gzip or DVZip and retains the
compressed temporary archive for upload. HTTP chunk headers, payload and CRLF share one TLS
write buffer. Cancellation and final receiver confirmation remain part of the transfer flow.

Run codec benchmarks and compression-backend checks:

```powershell
cargo run --release -p omdrop-proto --example codec_bench
cargo run --release -p omdrop-airdrop --example archive_bench
cargo run --release -p omdrop-airdrop --example archive_bench --features flate2/zlib-rs
native/owl/build/owl_test_radio_io.exe --bench
cargo test -p omdrop-airdrop -p omdrop-core --features flate2/zlib-rs
```

Host benchmarks measure codec CPU cost. Measure wireless performance on hardware using a fixed
board, channel and file, recording elapsed time, file integrity and radio drop counters.
