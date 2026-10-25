# Unified build and packaging

[简体中文](PACKAGING.zh-CN.md) · [Environment setup](SETUP.md) · [Development guide](DEVELOPMENT.md)

Rust `wayline-tools` implements the same build and packaging commands on Windows and Linux.
Prepare the native Rust and ESP-IDF environments, then run from the repository root.

## Build TUI

Windows, Linux and WSL share the same entry point, also available as `just tui`:

```sh
cargo run --locked -p wayline-tools -- tui
```

The UI defaults to Simplified Chinese. Press `L` to switch between Chinese and English, or
select the initial language explicitly:

```sh
cargo run --locked -p wayline-tools -- tui --lang en-US
```

Use `--lang zh-CN` for Chinese. `--preview` and `--help` also accept `--lang`. Language selection
lasts for the current session. Switching updates menus, setting labels, current status and
notices while preserving the task and settings. Outside text editing, either `l` or `L` switches
language, including while a task runs. In the editor, `l/L` inserts text; save or leave the editor
before switching. Existing logs and underlying tool diagnostics retain their original text.

At startup the TUI reads [zh-CN.toml](../crates/wayline-tools/locales/zh-CN.toml) and
[en-US.toml](../crates/wayline-tools/locales/en-US.toml) from the repository. Restart after editing
translations; recompilation is not required. Both files must exist. Missing/unknown fields,
incorrect menu array lengths or malformed TOML report the file path before entering the UI.
Preserve key names and array ordering when translating. Underlying tool diagnostics are not
part of these files.

Use a terminal of at least 90 columns and 26 rows; Chinese mode needs a suitable font.
Select a task on the left and configure board, host
Debug/Release, host tests, package platform and paths on the right. Command arguments, live
logs, elapsed time and actual exit status appear below. For a complete package, run the tasks
in order: environment check, host build, firmware build, full package, release verification.
Firmware-only and tool-only packaging are also available. Only one task runs at a time.

| Key | Action |
| --- | --- |
| `Tab` / `Shift+Tab` | Switch task, settings and log panes |
| `l` / `L` | Switch Chinese / English outside text editing, including while a task runs |
| `Up` / `Down` | Select a task or setting; scroll to additional settings |
| `Enter` | Run the selected task or edit a setting |
| `Left` / `Right` | Cycle board/mode/test/platform choices; scroll logs horizontally |
| `Enter` / `Esc` / `Ctrl+U` | Save, discard or clear an edit; Unicode, spaces, paste and cursor movement are supported |
| `PgUp` / `PgDn` / `End` | Browse logs or resume following the latest output |
| `s` | Save the retained log snapshot under `.cache/tui-logs/` |
| `x` | Cancel the task and clean up its process tree |
| `q` / `Ctrl+C` | Quit, first cancelling and waiting for any running task |

Settings last for the current session. The default host path uses Cargo's `target_directory`,
including `CARGO_TARGET_DIR` and configured `target-dir`, plus the selected Debug/Release profile.
When using a custom target triple, set the host directory to include that triple. Firmware
directories default to separate OS/board paths and can be overridden with an existing build.
The output setting is used for packaging and full-package verification. New packages require
a nonexistent output directory; choose another directory for subsequent packages. Selecting
a package platform does not cross-compile the host programs.

The TUI inherits its terminal environment. Activate ESP-IDF before firmware work; the settings
can override `idf.py` and Python paths. Host Debug/Release does not change firmware optimization,
which remains controlled by SDK defaults. Windows TUI uses Windows tools; WSL TUI uses Linux
tools. For offline packaging, set the cache and official archive paths; hash checks still apply.
Linux host builds exclude the unported desktop and Windows services.

The UI retains the latest 2000 log lines, splits excessively long lines and reports how many
older lines were omitted. Saving logs exports that retained snapshot. Settings are locked
while a task runs. Cancellation may leave incremental build outputs. The TUI does not install
services or flash devices. Use the subcommands below for automation, or render a static preview
without an interactive terminal or starting any task:

```sh
cargo run --locked -p wayline-tools -- tui --preview
```

## Build host, firmware and directory package

For Cardputer Adv, use a new output directory:

```sh
cargo run --locked -p wayline-tools -- host-build --release
cargo run --locked -p wayline-tools -- firmware-build --board cardputer-adv
cargo run --locked -p wayline-tools -- package-build --host-dir target/release --board cardputer-adv --output dist/wayline --version 0.1.0
cargo run --locked -p wayline-tools -- release-verify dist/wayline
```

`firmware-build --board` selects the chip, defaults and component-manager mode.
Its default output is `firmware/build-windows-cardputer-adv` or
`firmware/build-linux-cardputer-adv`, keeping Windows/WSL CMake caches separate.
`package-build` finds firmware using the same rule and defaults to the current OS's x64 tools.

| Board | Chip | Interface |
| --- | --- | --- |
| `cardputer-adv` | ESP32-S3 | Native USB, Cardputer UI |
| `esp32s3-uart0` | ESP32-S3 | USB-UART / UART0, headless |
| `esp32s3-native-usb` | ESP32-S3 | Native USB, headless |
| `esp32c5-uart0` | ESP32-C5 | USB-UART / UART0, headless |
| `esp32c5-native-usb` | ESP32-C5 | Native USB, headless |

The current headless S3 SDK defaults select 16 MB, while the host board profiles still declare
8 MB. Packaging explicitly rejects this mismatch; align the board profiles and SDK defaults
before distributing these images. Capacity checks are never bypassed.

For custom build directories, pass the matching `--firmware-build` when packaging:

```sh
cargo run --locked -p wayline-tools -- firmware-build --board esp32c5-native-usb --output firmware/build-c5
cargo run --locked -p wayline-tools -- package-build --host-dir target/release --firmware-build firmware/build-c5 --board esp32c5-native-usb --output dist/wayline-c5 --version 0.1.0
```

Do not combine `--board` with low-level `--target`, `--sdkconfig-defaults` or
`--no-component-manager`. Low-level options remain available for custom configurations.
If using `CARGO_TARGET_DIR`, point `--host-dir` at the actual host output directory.

## Contents and validation

Windows packages include desktop, bridge, service, CLI, serial diagnostics, pinned Wintun,
the selected firmware and Windows esptool. Linux packages include CLI, serial diagnostics,
the selected firmware and Linux esptool; desktop, Windows services and Wintun are not ported.
`--platform windows-x64|linux-x64` selects the package target without cross-compiling;
input host binaries must have the requested executable format and architecture.

Cardputer firmware is stored under `firmware/manifest.json`, accompanied by `bootloader.bin`,
`partition-table.bin` and `owl-radio.bin`. Other boards use `firmware/profiles/<board>/`.
One directory package selects one board. Tools are under `tools/esptool/`, including the
official distribution's license and `tool.json`. Root-level `package-info.json` records the
platform and available capabilities; `release.json` hashes every packaged file.

Firmware packaging reads ESP-IDF's `project_description.json`, `flasher_args.json`,
`sdkconfig` and images, generates offsets, lengths and SHA-256 hashes, and invokes the runtime
`wayline-flash` validator for partition, chip and capacity validation. Version/source labels
preserve CMake's `project_version`, including its dirty suffix, rather than substituting the
packager checkout's unrelated Git HEAD.

esptool is pinned to the application's required **5.0.2**; Windows Wintun is pinned to
**0.14.1**. Downloads are verified against built-in SHA-256 values before bounded extraction.
Invalid hashes, escaping paths, links, duplicate files, excessive sizes and existing output
directories fail explicitly. Native tools also run `esptool version`; foreign-platform
packages verify the pinned archive and executable architecture and record
`nativeVersionChecked: false` in `tool.json`.

Files are prepared in a temporary directory and published only on success. Host inputs are
selected executables, not Cargo dependencies, incremental caches or PDBs. Outputs are directory
packages, not service installers. Service registration, flashing and device acceptance are
separate operations.

## Cache and offline archives

Downloads are cached in `.cache/downloads`; use `--cache-dir` for a shared location.
Standard `HTTP_PROXY` / `HTTPS_PROXY` variables configure network proxies. Corrupt cached
archives fail rather than being silently accepted or skipping verification.

Supply original official archives for offline packaging, not pre-extracted directories:

```sh
cargo run --locked -p wayline-tools -- package-build --host-dir target/release --board cardputer-adv --output dist/wayline-offline --version 0.1.0 --esptool-archive downloads/esptool-v5.0.2-windows-amd64.zip --wintun-archive downloads/wintun-0.14.1.zip
```

On Linux, use `esptool-v5.0.2-linux-amd64.tar.gz` and omit `--wintun-archive`.
Assembly and validation use the same Rust implementation.

Firmware and tool directories can also be generated separately:

```sh
cargo run --locked -p wayline-tools -- firmware-package --board cardputer-adv --output build/firmware-package
cargo run --locked -p wayline-tools -- package-verify build/firmware-package/manifest.json
cargo run --locked -p wayline-tools -- tool-package --output build/esptool
```

`firmware-package --build-dir` accepts existing IDF output; `tool-package --archive` accepts
an offline archive. The original `package-build --input DIR` remains a generic directory-copy
and hash-manifest operation; use `--host-dir` mode to assemble firmware and tools automatically.
