# Omdrop quick start

[简体中文](QUICKSTART.zh-CN.md)

You need Windows 10/11 x64, a PC with Bluetooth LE advertising, M5Stack Cardputer Adv and a USB data cable.

## Install and update

Extract the complete offline package, run `install.cmd`, and allow service installation.
Desktop opens as a normal user so Explorer drag-and-drop works. Firmware, flashing tools
and runtime libraries are bundled; no development environment or runtime downloads are needed.

Before updating, finish transfers, quit Omdrop from the tray, and close CLI commands.
The installer verifies all files before replacement and keeps the previous programs as a backup.
Received files, device name and identity settings are preserved.

## Initialize Cardputer Adv

1. Connect the board with a data cable and open Device firmware at the top of the sharing card.
2. Select the correct serial port and confirm Cardputer Adv.
3. For first use, choose First install / Reinstall. This replaces the program and partition
   layout and may erase data from other firmware. For Omdrop updates, choose Preserve settings;
   this requires recognizable firmware and an identical partition layout.
4. Start installation or update. Wait for writing, verification and the boot check to finish
   and for the device to report ready.

Keep the board connected. Closing the card only hides it; tray Quit asks whether to cancel.
Cancellation or power loss after writing begins may require a complete reflash; automatic
rollback is unavailable.

## Transfers and settings

Enable Bluetooth on the PC. On iPhone, enable Wi-Fi and Bluetooth and set AirDrop to Everyone
for 10 Minutes. The PC's built-in Wi-Fi can stay off; no shared router connection is needed.

- Send: drop files, or select them from the tray, with `Ctrl+O`, or through Explorer's
  Send with Omdrop menu. Select a recipient and wait for Delivered after uploading reaches 100%.
- Receive: select this PC in the Apple device's AirDrop list, then accept in Omdrop.
- Settings: change the device name or receive folder shared by desktop and CLI.
  The default is Downloads/Omdrop. Changing it leaves existing files in place and is blocked
  while receiving.
- Error details: hover over a notice to expand it, then use Copy to get complete diagnostics.

× or Esc hides the card while receiving continues. Tray Quit or `Ctrl+Q` ends this client's
tasks. The default on-demand service reclaims the radio after the last client exits;
a standalone service deployment can remain resident.

First launch registers Explorer and Send to → Omdrop AirDrop entries. On Windows 11 these
usually appear under Show more options. After quitting, run
`omdrop-desktop.exe --unregister-shell` to remove them.

## Board interface

The board supports Simplified Chinese, Traditional Chinese and English, with Status, Sharing
guide, Preferences and More pages. Simultaneous transfers have separate cards.
Connected means a recent host command was received; use transfer cards for file results.

| Key | Action |
| --- | --- |
| Tab or 1 / 2 / 3 / 4 | Switch pages |
| L | Change language |
| B / M | Change brightness / toggle motion |
| A | Open sound settings; toggle task alerts on that page |
| V | Change and preview sound |
| − / + (or =) | Adjust sound-page volume by 5% |
| P | Preview sound on the sound page |
| T | Set display timeout: off, 30 seconds, 2 minutes or 5 minutes |
| S | Sleep the display; any key wakes it |
| H / I | Connection help / device information |
| W | Replay the welcome animation |
| R (Settings or About) | Restore defaults; Enter confirms, any other key cancels |
| Backspace | Return to the previous level or Status |

Preferences save automatically. Wireless operation and sound alerts continue during display
sleep. New transfers wake the display; active transfers suspend automatic sleep.

## Optional CLI toolkit

Extract the CLI toolkit and run it from PowerShell. Wireless transfers require an installed
desktop or standalone service package. The standalone package also installs with `install.cmd`
and runs without a UI.

```powershell
./omdrop-cli.exe --help
./omdrop-cli.exe status --json
./omdrop-cli.exe discover
./omdrop-cli.exe send --to "iPhone" -- "C:\Users\Example\Pictures\photo.jpg"
./omdrop-cli.exe receive
```

Receiving asks for each request by default; unattended receiving requires explicit
`--accept-all`. Ctrl+C ends the command and releases its session. See the
[development guide](DEVELOPMENT.md#cli-and-radio-diagnostics) for arguments, JSON and exit codes.

## Troubleshooting

- No device found: check the data cable and refresh ports. If needed, hold G0 and press RST
  to enter download mode.
- Boot failure after writing: release G0, press RST, keep the board connected and retry.
- Port busy: close other CLI transfers, serial monitors or flashing software.
- Drag-and-drop unavailable: launch desktop without elevation.
- Still blocked after reflashing on a new port: success on the new port does not clear the
  old port's failure record; see [maintenance recovery](DEVELOPMENT.md#service-configuration-and-ipc).

[Back to project overview](../README.md)
