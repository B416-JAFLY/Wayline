# Windows / Linux development setup

[简体中文](SETUP.zh-CN.md) · [Development guide](DEVELOPMENT.md)

Once configured, follow [unified packaging](PACKAGING.md) to build host programs, firmware and directory packages with flashing tools.

This guide is for developers building from source. Users of prebuilt applications and firmware
can follow the [quick start](QUICKSTART.md) without setting up a development environment.

Run native build commands from the repository root. Host applications and ESP32 firmware are separate targets:

| Development target | Windows x64 | Linux x64 |
| --- | --- | --- |
| CLI, shared libraries, protocol and firmware logic tests | Native Cargo | Native Cargo |
| Desktop, Windows service, Wintun bridge | Native MSVC | Not ported |
| ESP32-S3 / ESP32-C5 firmware | Native ESP-IDF | Native ESP-IDF |

Host tests do not need ESP-IDF. Flashable firmware needs ESP-IDF and the Rust `esp` toolchain; users of prebuilt firmware do not need the build SDK.

The steps below use host Rust **1.99.0**. Firmware development additionally uses ESP-IDF
**6.0.3**, espup **0.17.1**, and Xtensa Rust **1.97.0.0**. The IDF version matches
`firmware/dependencies.lock`.

## Native Windows setup

### Host applications

Install Git, [Rustup](https://rustup.rs/), and Visual Studio 2022 Build Tools with Desktop development with C++, MSVC x64/x86 tools and the Windows SDK. In Developer PowerShell, run from the repository root:

```powershell
rustup toolchain install 1.99.0 --profile minimal --component rustfmt,clippy
if ($LASTEXITCODE -ne 0) { throw 'Rust toolchain installation failed' }
rustup override set 1.99.0
if ($LASTEXITCODE -ne 0) { throw 'Rust toolchain selection failed' }
cargo run --locked -p wayline-tools -- host-build --test
if ($LASTEXITCODE -ne 0) { throw 'Host build or tests failed' }
```

Omitting `--release` builds Debug binaries. For desktop only:

```powershell
cargo run --locked -p wayline-tools -- desktop-build
if ($LASTEXITCODE -ne 0) { throw 'Desktop build failed' }
# Launch from an unelevated terminal so Explorer drag-and-drop works.
.\target\debug\wayline-desktop.exe
```

If GPUI cannot find `fxc.exe`, ensure the Windows SDK is installed and set its actual path:

```powershell
$env:GPUI_FXC_PATH = 'C:\Program Files (x86)\Windows Kits\10\bin\<SDK-version>\x64\fxc.exe'
Test-Path -LiteralPath $env:GPUI_FXC_PATH
```

Builds do not update an installed service or prepare a complete offline release package.

### Firmware

Use Espressif's [ESP-IDF Installation Manager](https://docs.espressif.com/projects/esp-idf/en/release-v6.0/esp32s3/get-started/windows-setup.html) to install **6.0.3** and the ESP32-S3 / ESP32-C5 tools. Open its ESP-IDF PowerShell terminal. For an existing manual installation, run that installation's `export.ps1` in the current session. Setting only `IDF_PATH` does not activate the Python environment or cross-compilers.

```powershell
cargo install espup --version 0.17.1 --locked
if ($LASTEXITCODE -ne 0) { throw 'espup installation failed' }
espup install --targets esp32s3,esp32c5 --std --toolchain-version 1.97.0.0 --stable-version 1.99.0
if ($LASTEXITCODE -ne 0) { throw 'ESP Rust toolchain installation failed' }
cargo run --locked -p wayline-tools -- firmware-check
if ($LASTEXITCODE -ne 0) { throw 'Firmware environment check failed' }
cargo run --locked -p wayline-tools -- firmware-build --target esp32s3 --output firmware/build-rust --sdkconfig-defaults "sdkconfig.defaults;sdkconfig.defaults.cardputer"
if ($LASTEXITCODE -ne 0) { throw 'Firmware build failed' }
```

Install tools once, but activate IDF in every new terminal. Do not share CMake build directories between operating systems.

## Native Linux setup

For Debian / Ubuntu, install the packages below; use equivalent development packages on other distributions. In WSL, prefer a checkout in the Linux filesystem to reduce compilation overhead.

```bash
sudo apt-get update
sudo apt-get install -y git curl build-essential pkg-config libudev-dev libssl-dev
```

Install Rust using [Rustup](https://rustup.rs/), then run:

```bash
rustup toolchain install 1.99.0 --profile minimal --component rustfmt,clippy
rustup override set 1.99.0
cargo run --locked -p wayline-tools -- host-build --test
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
```

Serial enumeration needs `libudev-dev`, not just the libudev runtime. The CLI and shared crates support development, host tests and local protocol verification; Windows service, BLE/Wintun integration and the full wireless workflow still need platform implementations.

For firmware, also install ESP-IDF (once):

```bash
sudo apt-get install -y python3 python3-venv python3-pip cmake ninja-build ccache flex bison gperf libffi-dev libusb-1.0-0-dev unzip xz-utils
mkdir -p "$HOME/esp"
git clone --branch v6.0.3 --recursive https://github.com/espressif/esp-idf.git "$HOME/esp/esp-idf-v6.0.3"
"$HOME/esp/esp-idf-v6.0.3/install.sh" esp32s3,esp32c5
cargo install espup --version 0.17.1 --locked
espup install --targets esp32s3,esp32c5 --std --toolchain-version 1.97.0.0 --stable-version 1.99.0
```

Activate the environment in each build terminal, then return to the repository root:

```bash
. "$HOME/.cargo/env"
. "$HOME/esp/esp-idf-v6.0.3/export.sh"
. "$HOME/export-esp.sh"
cargo run --locked -p wayline-tools -- firmware-check
cargo run --locked -p wayline-tools -- firmware-build --target esp32s3 --output firmware/build-linux-cardputer --sdkconfig-defaults "sdkconfig.defaults;sdkconfig.defaults.cardputer"
```

For ESP32-C5 native USB firmware, use a separate output directory and headless defaults:

```bash
cargo run --locked -p wayline-tools -- firmware-build --target esp32c5 --output firmware/build-linux-esp32c5-usb --sdkconfig-defaults "sdkconfig.defaults;sdkconfig.defaults.esp32c5;sdkconfig.defaults.esp32c5-usb" --no-component-manager
```

`--no-component-manager` excludes M5Unified/M5GFX in headless builds and prevents the C5
target from rewriting the S3 component lockfile. Outputs include the application image,
bootloader and partition table. Successful compilation does not verify flashing or device interoperability.

## Environment checks and troubleshooting

`firmware-check` checks Python/IDF startup, CMake, Ninja, Rust `esp` and `rust-src`, returning nonzero on failure. It does not download tools, access a serial port or certify firmware compilation or hardware behavior.

`firmware-build` and `firmware-check` select IDF from `--idf`, then `IDF_PATH/tools/idf.py`, then `idf.py` on `PATH`. Python comes from `--python`, then `IDF_PYTHON_ENV_PATH`, then the platform's `python` / `python3`. Invalid explicit paths fail instead of silently selecting a different SDK.

```powershell
cargo run --locked -p wayline-tools -- firmware-check --idf 'C:\esp\esp-idf-v6.0.3\tools\idf.py' --python 'C:\esp\python-env\Scripts\python.exe'
```

Replace these paths with the actual installation. Activate IDF first to expose cross-compilers and other tools. `firmware-build` supports executable custom `--idf` wrappers; `firmware-check` requires the actual `idf.py` so a wrapper's own version cannot be mistaken for a working SDK. Only `.py` scripts accept `--python`.

| Error | Action |
| --- | --- |
| Missing `idf.py` / `IDF_PATH` | Activate IDF in this terminal or check explicit paths |
| WSL script cannot find `cargo` | Source `$HOME/.cargo/env`; non-login shells may not load user shell configuration |
| Missing Python packages or virtual environment | Activate the matching IDF installation; rerun its install/repair steps |
| Missing `esp` or `rust-src` | Repeat the pinned espup installation; source `export-esp.sh` on Linux |
| Missing `libudev.pc` | Install `libudev-dev` and `pkg-config` |
| CMake cache references another machine or OS | Use a separate `--output` directory for each machine and OS |
| GitHub dependency downloads time out in WSL | Check network and proxy access from WSL itself; a Windows loopback proxy address cannot be reused directly in WSL NAT mode |

Preserve lockfiles and verification. Experimental RX aggregation requires a specific Wi-Fi library SHA-256; a matching SDK version string alone does not establish ABI compatibility. Do not bypass this check. Board, flash capacity and package validation still follow the selected profile.
