# omdrop-owl-windows

AirDrop between Windows and Apple devices, with a USB-attached ESP32 as the Wi-Fi radio.

The desktop application, Windows radio supervisor, AirDrop core and ESP32 application are
implemented in Rust. OWL supplies the AWDL engine in C; ESP-IDF and M5Unified supply the
firmware's hardware drivers.

The supported firmware target is ESP32-S3, with M5Stack Cardputer Adv as the validated board.
It uses native USB Serial/JTAG and AWDL channel 6 (2.4 GHz). ESP32-C5 support is not implemented
in this Rust firmware. The PC needs Bluetooth LE advertising for peer wake-up; its built-in
Wi-Fi is not used.

[中文说明](README.zh-CN.md) · [Rust development guide](docs/DEVELOPMENT.md)

## ESP32 UI demo

![Cardputer Adv interface walkthrough](docs/assets/cardputer-demo.gif)

This 53-second loop covers the boot logo, connection sphere, sharing guide, preferences,
sound settings, help, device information, all three languages, and sending, simultaneous
transfers, completion and interruption. It uses the actual firmware renderer with simulated
transfer events.

## AirDrop → ESP32 → Omdrop

![AirDrop through ESP32 to Omdrop interaction demo](docs/assets/airdrop-esp32-omdrop.gif)

This 24-second walkthrough simulates choosing a photo in iPhone Photos, tapping Share,
selecting AirDrop and Omdrop PC in the system sheets, then desktop approval, transfer,
final confirmation and saving to Downloads/Omdrop. ESP32 bridges the wireless connection;
Omdrop handles acceptance and file storage. Phone and desktop panels are illustrative;
the board uses the actual firmware renderer. Transfer data is simulated.

## Omdrop → ESP32 → AirDrop

![Omdrop sending a photo through ESP32 to iPhone](docs/assets/omdrop-esp32-airdrop.gif)

This 24-second walkthrough simulates choosing a photo on Windows, selecting a nearby iPhone,
clicking Send, and accepting the AirDrop request on iPhone. The photo travels through
USB → ESP32 → AWDL. Omdrop waits for receiver confirmation before reporting delivery,
and iPhone displays the received photo. Phone and desktop panels are illustrative;
the board uses the actual firmware renderer. Transfer data is simulated.

## Source layout

| Path | Purpose |
| --- | --- |
| `Cargo.toml`, `Cargo.lock` | Windows workspace and locked dependency versions |
| [`assets/branding`](assets/branding/README.md) | Shared logo SVG masters, PNG previews and design notes for desktop and firmware |
| `apps/desktop` | GPUI desktop, tray, Explorer integration, send and receive UI |
| `crates/omdrop-service` | Supervisor library linked into desktop, plus an optional standalone Rust service |
| `crates/omdrop-host` | Shared preferences, identity, receiver advertisement IDs, save directory and AWDL selection |
| `crates/omdrop-flash` | Firmware validation, maintenance coordination, flashing and boot checks |
| `crates/omdrop-core` | AirDrop discovery, BLE wake-up, TLS and transfer workflows |
| `crates/omdrop-airdrop`, `crates/omdrop-net` | Archive/plist codecs and DNS/HTTP primitives |
| `crates/omdrop-board`, `crates/omdrop-proto` | Board feedback and shared host/firmware wire protocol |
| `apps/cli`, `crates/omdrop-radio`, `crates/omdrop-transport` | CLI, serial diagnostics and stream adapters |
| `firmware` | Independent Rust/ESP-IDF project: radio application, UI, audio, fonts and translations |
| `firmware/components/radio` | ESP-IDF/M5Unified C/C++ bridges and component build files |
| `native/owl` | Windows OWL adapter, Wintun integration and regression tests |
| `native/owl/patches`, `native/owl/pins` | OWL fixes and pinned upstream revisions |
| `vendor/stacksafe-macro` | Rust compatibility patch for GPUI's indirect macro dependency |
| `scripts`, `tests`, `justfile` | Build, install, diagnostic and test entrypoints |

The shared C wire definitions live beside the Rust protocol crate in
`crates/omdrop-proto/include/proto.h`. Firmware is excluded from the Windows workspace so its
Xtensa build and host tests retain their own Cargo lockfile and compilation profile.

Build outputs, downloaded `.cache/upstream/` sources and ESP-IDF managed components are ignored
and regenerated from the dependency manifests and lockfiles.

## Install a prebuilt package

A complete Windows x64 package contains desktop, OWL, Wintun, Cardputer Adv firmware and a
pinned standalone flashing tool, plus required runtime libraries. Extract it and run `install.cmd`. Installation verifies the
package, elevates service registration, then starts desktop from the original unelevated process.
Installation and flashing use the bundled files without development tools or runtime downloads.
The optional CLI toolkit uses an installed desktop or standalone service.
See the [Chinese quick start](docs/QUICKSTART.zh-CN.md).

Quit desktop and finish CLI commands before updating. The installer keeps a backup of the old
program directory; preferences, identity and received files remain outside that directory.
Clean Windows installation and physical-device acceptance remain required before a general
release; see the [implementation and acceptance plan](docs/CLI_DESKTOP_PLAN.zh-CN.md).

## Build and install from source on Windows

Prerequisites: Git, Rust with the MSVC toolchain, Visual Studio C++ Build Tools and Windows SDK
(including `fxc.exe`). Building OWL additionally requires MinGW-w64 GCC on PATH, plus CMake
and Ninja. The OWL script discovers CMake/Ninja from PATH, standard CMake/ESP-IDF installations
and the Visual Studio CMake component. Custom locations can be supplied with `-CMakePath`
and `-NinjaPath`; it does not change your PATH.

From the repository root:

```powershell
scripts/build-owl.ps1
scripts/build-release.ps1
```

The OWL script fetches the revision in `native/owl/pins`, applies `native/owl/patches/owl-*.patch`, downloads and
checksum-verifies Wintun, then builds `native/owl/build/owl.exe` and `wintun.dll`.
`build-release.ps1` builds desktop, CLI and the standalone service, then records versions and
checksums for packaging. For desktop-only development, use `scripts/build-desktop.ps1`.
Host executables are written to `target/release`.

Quit any installed desktop from its tray menu, then run the installer in an administrator
PowerShell terminal:

```powershell
scripts/install-service.ps1
```

This developer installer uses prebuilt binaries; pass `-Build` to explicitly compile first.
It installs these files together; firmware and flashing tools are provided by the full release package:

```text
%ProgramFiles%\omdrop-owl\
    omdrop-desktop.exe
    owl.exe
    wintun.dll
```

Launch the installed desktop normally, without elevation, so Explorer drag-and-drop works.
Windows starts the same EXE with `--service --on-demand` in a separate privileged process.
Desktop and CLI hold usage sessions. Tray Quit or `Ctrl+Q` cancels desktop's transfers and receiving;
the service continues while another client is using it. Releasing the final session waits for
OWL reclamation and stops the on-demand service. Closing the sharing card only hides it.

`owl.exe` is managed automatically.
For a headless deployment, `scripts/install-service.ps1 -Runtime Rust` installs the standalone
Rust supervisor with automatic startup. `-Uninstall` removes the registered service and leaves
installed files in place.

The desktop supports live peer discovery, file selection/drop, receive approval and transfer
feedback. Its homepage point sphere rotates and breathes while the wireless bridge is ready,
dims to rest when disconnected, and pauses when the card is hidden.
The paired-paper icon appears in the app and Windows shell. Its tray variant is blue when
the wireless bridge is ready and gray during startup, disconnection or connection errors,
including while the sharing card is hidden.
Window corners retain smooth alpha edges, and header icons are rasterized from the vector
master at the current display scale.
Receive feedback distinguishes waiting for the sender, transfer progress and final confirmation.
Known totals show a percentage bar; unknown totals show received bytes and an activity bar.
Completion requires protocol validation and the final response. Received files default to
Downloads/Omdrop. Choose another folder under Settings → Receive AirDrop → Change save directory.
Desktop and CLI share this preference; CLI `--save` overrides it for the current receive command.
Changing the folder leaves existing files in place and is unavailable during an active receive.
Receive notices use a short, dismissible banner that disappears after eight seconds. New receive
prompts and file selections clear old notices. The file-selection button stays visible, while
complete error details remain in the receiver log.
Hover over a notice to expand its full text, scroll long details, or click Copy to copy everything.
Automatic dismissal pauses while hovering over the notice or its details and restarts on leaving.
The Device firmware button at the top of the sharing card opens a firmware page
with explicit port and board selection, install/update, progress, cancellation and retry.
Preserving settings requires recognizable Omdrop firmware and an identical partition layout.
An unavailable application version is shown as unknown.

If a failed flash re-enumerates on another COM port, successful flashing on the new port does
not clear the old port's failure record. Automatic radio discovery stays paused until the verified
new port is explicitly configured or the old device is checked and its record handled. This
recovery path still requires hardware acceptance; see [service maintenance](docs/DEVELOPMENT.md#service-configuration-and-ipc).

The optional CLI shares desktop's name, identity and automatic AWDL selection. Wireless commands
start the installed service as needed. Interactive receiving asks for approval; unattended or
JSON receiving requires `--accept-all`, and unattended sending requires an unambiguous `--to`.
`status` and `doctor` do not start the service. Command examples, JSON Lines and exit codes are
documented in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#cli-and-radio-diagnostics).

The board's welcome animation uses the paired-paper icon and outlined Omdrop wordmark.
The folds settle together and the lettering fades in before the status page appears after
2.4 seconds. Any key skips it; W replays it. Disabling motion shows the complete logo immediately.
The idle status view uses a depth-shaded point sphere. Host activity drives slow rotation
and breathing; disconnecting dims it to rest.
The motion preference freezes the sphere, and other pages, transfer cards and display sleep
pause its animation.

## Build and flash Cardputer Adv

Install ESP-IDF 6.0 and the Xtensa Rust toolchain:

```powershell
cargo install espup --locked
espup install --targets esp32s3 --std
scripts/build-firmware.ps1
```

The script discovers an EIM ESP-IDF 6.0 installation, accepts an already activated IDF
environment, or takes `-IdfProfile <activation-script>`. Output is
`firmware/build-rust/owl-radio.bin`; a successful build also records image offsets and checksums
for packaging. See [offline release packages](docs/DEVELOPMENT.md#offline-release-packages)
for packaging commands. End users install firmware from desktop's firmware page.

Quit desktop and release the serial port before flashing. In an activated ESP-IDF terminal,
replace COM4 with the board's port:

```powershell
$fw = (Resolve-Path firmware).Path
idf.py -C $fw -B "$fw\build-rust" -D "SDKCONFIG=$fw\build-rust\sdkconfig" -p COM4 flash
python tests/firmware_regression.py -p COM4
```

`idf.py flash` writes the bootloader, partition table and application together. Stop an explicitly
started standalone OmdropOwl service before flashing or serial diagnostics. Reopen desktop
afterwards to start the radio again. The Cardputer controls and font workflow are documented
in [README.zh-CN.md](README.zh-CN.md) and [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Development checks

With `just` installed:

```powershell
just rust-test
just rust-check
just rust-firmware-test
just test-owl
```

`just build-firmware`, `just build-cardputer` and `just rust-firmware-build` use the same Rust
firmware build. With IDF activated, `just port=COM4 flash` or `just port=COM4 flash-cardputer`
flashes that output. Run `just --list` for all tasks.

Host protocol and service tests run without a physical radio. Apple-device transfers and
wireless throughput require hardware testing. ESP32-S3 uses channel 6; 5 GHz is unsupported.
Receive aggregation is disabled in normal builds. An [experimental firmware build](docs/DEVELOPMENT.md#experimental-receive-aggregation)
implements ADDBA negotiation and hardware Block Ack. USB uplink optimization has reduced queue
drops and improved measured aggregation receive speed; results still depend on the devices.

## Credits

Thanks to the authors, maintainers and contributors of the projects that provide Omdrop's
protocol research, implementation references, hardware support and development tools:

- **[omdrop-owl-windows](https://github.com/dantdmdarealguy/omdrop-owl-windows)** by
  dantdmdarealguy: this project's direct upstream, providing the Windows OWL port,
  ESP32 radio bridge, Wintun integration and initial AirDrop sender and receiver.
- **[omdrop-owl](https://github.com/t4t5/omdrop-owl)** by t4t5: the Linux radio backend,
  OWL patches and fixed-radio approach.
- **[omdrop](https://github.com/netmojo-systems/omdrop-plugin)** and
  **[omdrop-awdl](https://github.com/netmojo-systems/omdrop-awdl)** by Brent Kearney and contributors:
  AirDrop receiver, sender, mDNS/BLE tools and the radio-backend contract.
- **[OWL](https://github.com/seemoo-lab/owl)** by Milan Stute and the Open Wireless Link
  project (SEEMOO, TU Darmstadt), via [jedbillyb/owl](https://github.com/jedbillyb/owl):
  the userspace AWDL implementation.
- **[OpenDrop](https://github.com/seemoo-lab/opendrop)**: the open AirDrop implementation
  underlying the protocol used by the Rust AirDrop core.
- **[Windfall](https://github.com/UvejsGj/Windfall)** by UvejsGj (MIT): DVZip reverse engineering,
  used by the archive decoder in `crates/omdrop-airdrop`.
- **[GPUI](https://gpui.rs/)** by the Zed team: the GPU-accelerated Rust UI framework
  used by the desktop application.
- **[Wintun](https://www.wintun.net/)** by the WireGuard project: the Windows virtual
  network interface driver carrying AWDL traffic.
- **[ESP-IDF](https://github.com/espressif/esp-idf)** and
  **[esptool](https://github.com/espressif/esptool)** by Espressif, and
  **[espup](https://github.com/esp-rs/espup)** by the esp-rs community: ESP32 firmware
  drivers, flashing tools and Rust toolchain installation.
- **[M5Unified](https://github.com/m5stack/M5Unified)** and
  **[M5GFX](https://github.com/m5stack/M5GFX)** by M5Stack: Cardputer hardware access
  and display drivers.
- **[Noto CJK](https://github.com/notofonts/noto-cjk)**: the source fonts for the board's
  Chinese and English text. Font provenance, subset generation and licensing are recorded in
  [font-source.txt](firmware/assets/font-source.txt) and [OFL.txt](firmware/assets/OFL.txt).

Thanks also to maintainers across the Rust and wider open-source ecosystem, and contributors
who report issues, test devices and improve the project.

## License

GPL-3.0-or-later ([LICENSE](LICENSE)), as omdrop-owl and OWL are. Code ported from
omdrop-awdl, which is GPL-2.0-only, will stay in a separate program from OWL.
