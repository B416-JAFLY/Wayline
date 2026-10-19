# omdrop-windows

AirDrop between Windows and Apple devices through a USB-attached ESP32. The desktop app,
service, AirDrop core and firmware use Rust; OWL provides AWDL.

Supports Windows 10/11 x64 and M5Stack Cardputer Adv (ESP32-S3, native USB Serial/JTAG,
2.4 GHz channel 6). The PC needs Bluetooth LE advertising; ESP32 handles Wi-Fi traffic.
ESP32-C5 and 5 GHz are unsupported.

[中文说明](README.zh-CN.md) · [Quick start](docs/QUICKSTART.md) · [Development guide](docs/DEVELOPMENT.md)

## Engineering

- **Native desktop and Windows integration**: GPUI transfer UI, tray, Explorer context menus
  and drag-and-drop, with an unelevated UI separated from the privileged service.
- **Rust transfer core**: discovery, TLS/HTTP and CPIO/DVZip codecs shared by desktop and CLI,
  with streaming, archive validation, final confirmation, cancellation and failure cleanup.
- **Service and firmware maintenance**: named pipes and session leases coordinate clients,
  OWL lifecycle and exclusive serial access. Flashing isolates wireless work and verifies
  images and device startup before restoring readiness.
- **Embedded application**: Rust ESP32-S3 application and three-language Cardputer UI,
  a shared allocation-free CRC/COBS protocol, C/C++ hardware drivers and simultaneous
  send/receive feedback.
- **Builds and verification**: pinned OWL sources, patches and flashing tools, verified
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
2. Connect Cardputer Adv with a data cable. Open Device firmware, select its port and board,
   then install or update the firmware.
3. Enable Bluetooth on the PC. Enable Wi-Fi and Bluetooth on iPhone and set
   AirDrop to Everyone for 10 Minutes.
4. Drop files into Omdrop and select a recipient. Accept incoming transfers in Omdrop.

The package includes firmware, flashing tools and runtime libraries; no development tools
or runtime downloads are needed. Run desktop without elevation so Explorer drag-and-drop works.
Before updating, finish transfers, quit Omdrop from the tray, and close CLI commands.

- Received files default to Downloads/Omdrop; change the folder in Settings. Desktop and CLI
  share the device name, identity and save directory.
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
scripts/build-owl.ps1
scripts/build-release.ps1
```

Outputs are in `native/owl/build` and `target/release`. For developer installation, run
`scripts/install-service.ps1` in an administrator terminal. Firmware builds, tests and offline
packaging are covered in the [development guide](docs/DEVELOPMENT.md).

## Source layout

| Path | Purpose |
| --- | --- |
| `apps/desktop`, `apps/cli` | GPUI desktop and CLI entry points |
| `crates/omdrop-service` | Service, OWL lifecycle, sessions and maintenance |
| `crates/omdrop-core`, `crates/omdrop-host`, `crates/omdrop-flash` | Transfers, shared preferences and firmware operations |
| `crates/omdrop-airdrop`, `crates/omdrop-net` | Archive and network codecs |
| `crates/omdrop-proto`, `crates/omdrop-board`, `crates/omdrop-radio`, `crates/omdrop-transport` | Shared protocol, board feedback and transport adapters |
| `firmware` | Independent Rust/ESP-IDF project; hardware interfaces in `firmware/components/radio` |
| `native/owl` | Windows OWL, pinned versions and patches |
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
