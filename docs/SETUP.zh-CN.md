# Windows / Linux 开发环境

[English](SETUP.md) · [开发指南](DEVELOPMENT.zh-CN.md)

环境就绪后，按[统一构建与打包](PACKAGING.zh-CN.md)生成主机、固件及包含烧录工具的目录包。

本指南面向从源码构建的开发者。普通用户使用预编译程序和固件，按[快速开始](QUICKSTART.zh-CN.md)安装和使用，无需准备开发环境。

从仓库根目录执行原生构建命令。主机程序与 ESP32 固件是两套独立的构建目标：

| 开发目标 | Windows x64 | Linux x64 |
| --- | --- | --- |
| CLI、共用库、协议与固件逻辑测试 | 原生 Cargo | 原生 Cargo |
| 桌面、Windows 服务、Wintun 桥接 | 原生 MSVC 构建 | 尚未移植 |
| ESP32-S3 / ESP32-C5 固件 | 原生 ESP-IDF | 原生 ESP-IDF |

运行主机测试不需要 ESP-IDF。生成可烧录固件需要 ESP-IDF 和 Rust `esp` 工具链；普通用户使用预编译固件无需安装这些构建工具。

以下步骤使用主机 Rust **1.99.0**；固件开发额外使用 ESP-IDF **6.0.3**、espup **0.17.1** 和 Xtensa Rust **1.97.0.0**。IDF 版本与 `firmware/dependencies.lock` 对齐。

## Windows 原生开发

### 主机程序

安装 Git、[Rustup](https://rustup.rs/) 和 Visual Studio 2022 Build Tools，选择“使用 C++ 的桌面开发”、MSVC x64/x86 工具及 Windows SDK。在 Developer PowerShell 中从仓库根目录执行：

```powershell
rustup toolchain install 1.99.0 --profile minimal --component rustfmt,clippy
if ($LASTEXITCODE -ne 0) { throw 'Rust toolchain installation failed' }
rustup override set 1.99.0
if ($LASTEXITCODE -ne 0) { throw 'Rust toolchain selection failed' }
cargo run --locked -p wayline-tools -- host-build --test
if ($LASTEXITCODE -ne 0) { throw 'Host build or tests failed' }
```

不加 `--release` 默认生成 Debug 产物。仅构建桌面：

```powershell
cargo run --locked -p wayline-tools -- desktop-build
if ($LASTEXITCODE -ne 0) { throw 'Desktop build failed' }
# 之后从普通权限终端启动，保留 Explorer 拖放能力。
.\target\debug\wayline-desktop.exe
```

若 GPUI 找不到 `fxc.exe`，确认 Windows SDK 已安装，并设置实际 SDK 路径：

```powershell
$env:GPUI_FXC_PATH = 'C:\Program Files (x86)\Windows Kits\10\bin\<SDK版本>\x64\fxc.exe'
Test-Path -LiteralPath $env:GPUI_FXC_PATH
```

构建不会更新已安装服务，也不会自动准备完整离线发布包。

### 固件

使用 Espressif 的 [ESP-IDF Installation Manager](https://docs.espressif.com/projects/esp-idf/en/release-v6.0/esp32s3/get-started/windows-setup.html) 安装 **6.0.3** 和 ESP32-S3 / ESP32-C5 工具。打开安装器提供的 ESP-IDF PowerShell 终端；已有手动安装则在当前会话运行实际安装目录中的 `export.ps1`。只设置 `IDF_PATH` 不会自动激活 Python 虚拟环境和交叉编译器。

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

安装工具只需执行一次，但每个新的终端都需要激活 IDF。工作区与固件目录不要共用不同操作系统生成的 CMake 缓存。

## Linux 原生开发

以下包名适用于 Debian / Ubuntu；其他发行版安装对应开发包。在 WSL 中优先将源码检出到 Linux 文件系统，减少跨文件系统编译开销。

```bash
sudo apt-get update
sudo apt-get install -y git curl build-essential pkg-config libudev-dev libssl-dev
```

通过 [Rustup](https://rustup.rs/) 安装 Rust，然后执行：

```bash
rustup toolchain install 1.99.0 --profile minimal --component rustfmt,clippy
rustup override set 1.99.0
cargo run --locked -p wayline-tools -- host-build --test
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
```

`libudev-dev` 是串口枚举依赖，不能只安装 `libudev` 运行库。Linux 的 CLI 和共用库可用于开发、主机测试及本地协议验证；Windows 服务、BLE/Wintun 集成和完整无线工作流仍需平台实现。

构建固件时额外准备 ESP-IDF（仅首次安装）：

```bash
sudo apt-get install -y python3 python3-venv python3-pip cmake ninja-build ccache flex bison gperf libffi-dev libusb-1.0-0-dev unzip xz-utils
mkdir -p "$HOME/esp"
git clone --branch v6.0.3 --recursive https://github.com/espressif/esp-idf.git "$HOME/esp/esp-idf-v6.0.3"
"$HOME/esp/esp-idf-v6.0.3/install.sh" esp32s3,esp32c5
cargo install espup --version 0.17.1 --locked
espup install --targets esp32s3,esp32c5 --std --toolchain-version 1.97.0.0 --stable-version 1.99.0
```

每个构建终端激活环境后，回到仓库根目录：

```bash
. "$HOME/.cargo/env"
. "$HOME/esp/esp-idf-v6.0.3/export.sh"
. "$HOME/export-esp.sh"
cargo run --locked -p wayline-tools -- firmware-check
cargo run --locked -p wayline-tools -- firmware-build --target esp32s3 --output firmware/build-linux-cardputer --sdkconfig-defaults "sdkconfig.defaults;sdkconfig.defaults.cardputer"
```

ESP32-C5 原生 USB 固件使用独立目录和无屏配置：

```bash
cargo run --locked -p wayline-tools -- firmware-build --target esp32c5 --output firmware/build-linux-esp32c5-usb --sdkconfig-defaults "sdkconfig.defaults;sdkconfig.defaults.esp32c5;sdkconfig.defaults.esp32c5-usb" --no-component-manager
```

`--no-component-manager` 在无屏构建中排除 M5Unified/M5GFX，并避免用 C5 目标改写 S3 的组件锁文件。实际构建产物包含应用镜像、bootloader 和分区表；构建成功不代表已完成刷写或设备互通测试。

## 环境检查与故障定位

`firmware-check` 检查 IDF 能否由 Python 启动、CMake、Ninja、Rust `esp` 和 `rust-src`，失败返回非零退出码。它不下载依赖、不访问串口，且不证明固件已编译或硬件可用。

`firmware-build` / `firmware-check` 的 IDF 选择顺序为：`--idf`、`IDF_PATH/tools/idf.py`、`PATH` 中的 `idf.py`。Python 选择顺序为：`--python`、`IDF_PYTHON_ENV_PATH` 的解释器、当前平台的 `python` / `python3`。显式指定的错误路径会报错，不会静默切换 SDK。

```powershell
cargo run --locked -p wayline-tools -- firmware-check --idf 'C:\esp\esp-idf-v6.0.3\tools\idf.py' --python 'C:\esp\python-env\Scripts\python.exe'
```

将示例路径替换为真实安装路径；仍应先激活 IDF，以提供交叉编译器和其他依赖。`firmware-build` 支持可执行的自定义 `--idf` wrapper；`firmware-check` 要求实际的 `idf.py`，避免把 wrapper 自身版本误判为 SDK 可用。只有 `.py` 脚本接受 `--python`。

| 错误 | 检查项 |
| --- | --- |
| 找不到 `idf.py` / `IDF_PATH` | 在当前终端激活 IDF，或检查显式路径 |
| WSL 脚本找不到 `cargo` | 先加载 `$HOME/.cargo/env`；非登录 shell 不一定读取用户 shell 配置 |
| Python 缺包 / 虚拟环境路径不存在 | 使用该 IDF 安装对应的环境，重新运行其安装或修复步骤 |
| 找不到 `esp` / `rust-src` | 重新执行上述 espup 安装，并在 Linux 加载 `export-esp.sh` |
| `libudev.pc` 不存在 | 安装 `libudev-dev` 与 `pkg-config` |
| CMake 缓存指向另一台机器或操作系统 | 为每台机器和操作系统使用独立的 `--output` 目录 |
| WSL 下载 GitHub 依赖超时 | 检查 WSL 自身的网络和代理可达性；NAT 模式下 Windows 的回环代理地址不能直接照搬到 WSL |

保留依赖锁文件与校验。实验性 RX aggregation 还要求特定的 Wi-Fi 库 SHA-256，版本号相同也不代表 ABI 匹配；不要绕过该检查。板型、Flash 容量及固件包校验仍按对应 profile 约束执行。
