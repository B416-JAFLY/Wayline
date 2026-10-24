# omdrop-windows

AirDrop between Windows and Apple devices through a USB-attached ESP32. The desktop app,
service, AirDrop core and firmware use Rust. AWDL and AirDrop use the pinned Owly crates.

The default Rust AWDL backend based on Owly 0.4.0 can be built separately with
`scripts/build-bridge.ps1 -Test`. It uses the existing ESP32 firmware protocol and
Wintun and awaits device interoperability testing.
See the [development guide](docs/DEVELOPMENT.md#rust-awdl-backend).

Supports Windows 10/11 x64 and M5Stack Cardputer Adv (ESP32-S3, native USB Serial/JTAG,
2.4 GHz channel 6). The PC needs Bluetooth LE advertising; ESP32 handles Wi-Fi traffic.

A headless firmware build is available for ESP32-S3 and ESP32-C5 boards using UART0 USB-UART
or native USB Serial/JTAG. Desktop packages can include these profiles and select the matching link.
It switches at runtime between channel 6 (2.4 GHz) and channels 44 and 149 (5 GHz) on host
request, and its raw transmit rate is fixed to HE20/MCS0. C5 AirDrop interoperability and
throughput still need device validation. See the
[development guide](docs/DEVELOPMENT.md#headless-esp32-c5-build) for building and flashing it.

[中文说明](README.zh-CN.md) · [Quick start](docs/QUICKSTART.md) · [Development guide](docs/DEVELOPMENT.md)

[![Watch on YouTube](https://img.youtube.com/vi/8IJywijJMYU/hqdefault.jpg)](https://www.youtube.com/watch?v=8IJywijJMYU)

## Engineering

- **Native desktop and Windows integration**: GPUI transfer UI, tray, Explorer context menus
  and drag-and-drop, with an unelevated UI separated from the privileged service.
- **Rust transfer core**: discovery, TLS/HTTP and CPIO/DVZip codecs shared by desktop and CLI,
  with streaming, archive validation, final confirmation, cancellation and failure cleanup.
- **Service and firmware maintenance**: named pipes and session leases coordinate clients,
  radio lifecycle and exclusive serial access. Flashing isolates wireless work and verifies
  images and device startup before restoring readiness.
- **Embedded application**: Rust ESP32-S3 application and three-language Cardputer UI,
  a shared allocation-free CRC/COBS protocol, C/C++ hardware drivers and simultaneous
  send/receive feedback. A headless ESP32-C5 profile reuses the same protocol layer.
- **Builds and verification**: pinned Owly sources, adaptations and flashing tools, verified
  firmware and offline packages, host regression tests, protocol tests and UI previews.

## Interface and transfer demos

Demos use the actual firmware renderer with simulated transfers. Phone and desktop panels
are illustrative.

### ESP32 interface

![Cardputer Adv interface demo](docs/assets/cardputer-demo.gif)

Status, sharing guide, preferences, sound settings and three-language switching.

### Apple → Windows

![AirDrop through ESP32 to Omdrop](docs/assets/airdrop-esp32-omdrop.gif)

### Windows → Apple

![Omdrop through ESP32 to iPhone](docs/assets/omdrop-esp32-airdrop.gif)

## Install and use

1. Extract the complete Windows x64 offline package, run `install.cmd`, and allow service installation.
2. Connect Cardputer Adv with a data cable. Click the Device firmware icon at the top of the sharing card, select its port and board,
   then install or update the firmware.
3. Enable Bluetooth on the PC. Enable Wi-Fi and Bluetooth on iPhone and set
   AirDrop to Everyone for 10 Minutes.
4. Drop files into Omdrop and select a recipient. Accept incoming transfers in Omdrop.

The package includes firmware, flashing tools and runtime libraries; no development tools
or runtime downloads are needed. Run desktop without elevation so Explorer drag-and-drop works.
Before updating, finish transfers, quit Omdrop from the tray, and close CLI commands.

- Received files default to Downloads/Omdrop; change the folder in Settings. Desktop and CLI
  share the device name, identity and save directory.
- Receive results can appear in a separate, dismissible desktop notification window. Settings lets
  you enable or disable it, choose among six screen positions, and set a 3–30 second dismissal delay.
- Active send and receive cards show smoothed transfer speed and elapsed time. Timing starts
  when data transfer begins, includes final confirmation, and excludes preparation and acceptance waits.
- The history icon at the top of the sharing card opens Recent transfers with up to 200 results.
  The compact list shows device, outcome, file overview, size and time. Details shows the complete
  file list, transferred bytes, duration, average speed, radio band/channel and transfer mode,
  and each file's location. Records retain sent source paths and successful receive
  paths without file contents; older records may have no file details.
- Delete individual records or clear the history with confirmation every time. Also delete files
  and Remember this option are initially unchecked. Deleting files permanently removes the sent
  source files and received files in those records. Remembering updates the file-deletion default,
  also available in Settings; future deletions still require confirmation.
- Overflowing pages, lists and long text show a persistent proportional scrollbar, with fixed actions kept visible.
- Settings shows the identified board and supported radio channels: ESP32-S3 supports 2.4 GHz
  channel 6; ESP32-C5 also supports 5 GHz channels 44 and 149. CLI commands are `radio status`
  and `radio set`; see the [development guide](docs/DEVELOPMENT.md#cli-and-radio-diagnostics).
- The desktop language dropdown supports Simplified Chinese, Traditional Chinese, English,
  Japanese, Korean, French, German and Spanish. The choice is saved locally and updates the
  tray, system dialogs and Explorer entries.
- Uploading to 100% still requires receiver confirmation. Delivered means sending succeeded.
- × or Esc hides the card while receiving continues. Tray Quit or `Ctrl+Q` ends this client's
  tasks; the service stays active while another CLI session uses it.

First installation replaces the board's program and partition layout. Preserving settings
requires recognizable Omdrop firmware and an identical layout. See the
[quick start](docs/QUICKSTART.md) for board controls and troubleshooting.
CLI arguments, JSON output and exit codes are in the
[development guide](docs/DEVELOPMENT.md#cli-and-radio-diagnostics).

## Build from source

Prepare the [development environment](docs/DEVELOPMENT.md#build-and-test), then run PowerShell
from the repository root:

```powershell
scripts/build-release.ps1
```

Outputs, including `omdrop-bridge.exe` and verified Wintun, are in `target/release`. For developer installation, run
`scripts/install-service.ps1` in an administrator terminal. Firmware builds, including the
headless ESP32-S3 and ESP32-C5 builds, tests and offline packaging are covered in the
[development guide](docs/DEVELOPMENT.md).

## Source layout

| Path | Purpose |
| --- | --- |
| `apps/desktop`, `apps/cli` | GPUI desktop and CLI entry points |
| `apps/bridge`, `crates/omdrop-awdl` | Rust AWDL child, ESP32 serial and Wintun integration |
| `vendor/owly-awdl`, `vendor/owly-airdrop` | Pinned AWDL and AirDrop protocol implementations |
| `crates/omdrop-service` | Service, radio lifecycle, sessions and maintenance |
| `crates/omdrop-core`, `crates/omdrop-host`, `crates/omdrop-flash` | Transfers, shared preferences and firmware operations |
| `crates/omdrop-proto`, `crates/omdrop-board`, `crates/omdrop-radio`, `crates/omdrop-transport` | Shared protocol, board feedback and transport adapters |
| `firmware` | Independent Rust/ESP-IDF project; hardware interfaces in `firmware/components/radio` |
| `scripts`, `tests`, `assets/branding` | Builds, diagnostics and [branding assets](assets/branding/README.md) |

## Credits

Thanks to the authors, maintainers and contributors of:

- [omdrop-owl-windows](https://github.com/dantdmdarealguy/omdrop-owl-windows) (dantdmdarealguy):
  implementation foundation for the Windows OWL port, Wintun integration and ESP32 radio bridge.
- [omdrop-owl](https://github.com/t4t5/omdrop-owl) (t4t5): Linux radio backend and fixed-channel approach.
- [omdrop](https://github.com/netmojo-systems/omdrop-plugin),
  [omdrop-awdl](https://github.com/netmojo-systems/omdrop-awdl) (Brent Kearney and contributors)
  and [OpenDrop](https://github.com/seemoo-lab/opendrop) (SEEMOO): AirDrop protocol and transfer references.
- [OWL / Open Wireless Link](https://github.com/seemoo-lab/owl) (Milan Stute, SEEMOO / TU Darmstadt)
  and [jedbillyb/owl](https://github.com/jedbillyb/owl): userspace AWDL implementation.
- [Windfall](https://github.com/UvejsGj/Windfall) (UvejsGj): DVZip format research.
- [GPUI](https://gpui.rs/) (Zed) and [Wintun](https://www.wintun.net/) (WireGuard):
  desktop UI and virtual network interface.
- [ESP-IDF](https://github.com/espressif/esp-idf), [esptool](https://github.com/espressif/esptool)
  (Espressif), [espup](https://github.com/esp-rs/espup) (esp-rs),
  [M5Unified](https://github.com/m5stack/M5Unified) and [M5GFX](https://github.com/m5stack/M5GFX)
  (M5Stack): firmware tools and hardware support.
- [Noto CJK](https://github.com/notofonts/noto-cjk): board fonts; see
  [font-source.txt](firmware/assets/font-source.txt) and [OFL.txt](firmware/assets/OFL.txt).

## Community links

- [LINUX DO Community](https://linux.do/)

## License

See [LICENSE](LICENSE). Release packages include dependency licenses and notices.
