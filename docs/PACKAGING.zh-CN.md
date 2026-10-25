# 统一构建与打包

[English](PACKAGING.md) · [环境配置](SETUP.zh-CN.md) · [开发指南](DEVELOPMENT.zh-CN.md)

构建和打包统一由 Rust `wayline-tools` 实现，Windows 与 Linux 使用相同命令。先准备本机 Rust 和 ESP-IDF 环境，再从仓库根目录执行。

## 构建 TUI

Windows、Linux、WSL 使用同一入口，也可执行 `just tui`：

```sh
cargo run --locked -p wayline-tools -- tui
```

默认使用简体中文，按 `L` 即时切换中文 / English。也可指定启动语言：

```sh
cargo run --locked -p wayline-tools -- tui --lang en-US
```

`--lang zh-CN` 指定中文；`--preview` 和 `--help` 同样支持 `--lang`。语言选择仅在本次会话有效。
在非编辑状态按 `l` 或 `L` 切换菜单、配置标签、当前状态和提示，运行中的任务和配置保持原样。
编辑文本时 `l/L` 正常输入，保存或退出编辑后再切换语言；已有日志与底层工具诊断保留原文。

TUI 启动时从仓库中的 [zh-CN.toml](../crates/wayline-tools/locales/zh-CN.toml) 和
[en-US.toml](../crates/wayline-tools/locales/en-US.toml) 读取文案，修改后重启即可生效，无需重新编译。
两个文件都必须存在；缺失字段、未知字段、错误的菜单数组长度或 TOML 格式错误会在进入界面前报出文件路径。
维护翻译时保留键名及数组顺序，只有文案需要翻译；底层工具的原始诊断不在这两个文件中。

终端至少需要 90 列、26 行，并使用能显示中文的字体。左侧选择任务，右侧配置板型、主机
Debug/Release、主机测试开关、打包平台和路径；下方显示命令、实时日志、耗时与真实退出状态。
依次执行「检查固件环境 → 构建主机程序 → 构建固件 → 组装完整发布包 → 校验完整发布包」即可完成完整流程，
也可以单独打包固件或烧录工具。每次只运行一个任务，失败后可修改配置重试。

| 按键 | 操作 |
| --- | --- |
| `Tab` / `Shift+Tab` | 切换任务、配置、日志面板 |
| `l` / `L` | 非编辑状态下切换中英文，任务运行中也可使用 |
| `↑` / `↓` | 选择任务或配置；配置列表可向下滚动 |
| `Enter` | 执行选中的任务，或编辑配置 |
| `←` / `→` | 切换板型、模式、测试和平台；日志面板中横向滚动 |
| `Enter` / `Esc` / `Ctrl+U` | 编辑时保存、放弃、清空；支持中文、空格、粘贴与光标移动 |
| `PgUp` / `PgDn` / `End` | 翻阅日志 / 恢复跟随最新日志 |
| `s` | 将当前保留的日志保存到 `.cache/tui-logs/` |
| `x` | 取消当前任务并清理它启动的子进程 |
| `q` / `Ctrl+C` | 退出；运行中先取消并等待清理完成 |

配置仅在本次会话中有效。主机产物目录默认取 Cargo 的 `target_directory`，兼容
`CARGO_TARGET_DIR` 和 Cargo 配置的 `target-dir`，并随 Debug/Release 切换；使用自定义目标三元组时，
需手动选择包含该三元组的产物目录。固件构建目录默认按操作系统和板型分开，也可选用已有构建目录。
输出目录用于当前打包任务，也用于完整包校验；新建包时该目录必须不存在。重复打包前换一个输出目录。
打包平台只选择归档和二进制格式，不执行交叉编译。

TUI 继承启动终端的环境。固件构建仍需先激活 ESP-IDF；`idf.py` 和 Python 路径可在配置中覆盖。
主机 Debug/Release 不控制固件优化等级，固件仍由 `sdkconfig.defaults` 配置。Windows TUI 执行 Windows
工具链，WSL TUI 执行 Linux 工具链。离线打包可填写缓存目录和官方归档路径，仍会执行完整哈希校验。
Linux 主机构建不包含尚未移植的桌面与 Windows 服务。

界面保留最近 2000 行日志，单行超长内容会分段；更早的行数会明确显示。`s` 保存的是当前日志快照。
任务运行期间配置锁定，取消可能保留增量编译产物；TUI 不安装服务或刷写设备。
自动化使用本文的子命令；无交互终端时可输出静态预览，不启动任何任务：

```sh
cargo run --locked -p wayline-tools -- tui --preview
```

## 构建主机、固件及目录包

以下以 Cardputer Adv 为例；输出目录必须不存在：

```sh
cargo run --locked -p wayline-tools -- host-build --release
cargo run --locked -p wayline-tools -- firmware-build --board cardputer-adv
cargo run --locked -p wayline-tools -- package-build --host-dir target/release --board cardputer-adv --output dist/wayline --version 0.1.0
cargo run --locked -p wayline-tools -- release-verify dist/wayline
```

`firmware-build --board` 自动选择芯片、默认配置和组件管理模式。默认固件目录为
`firmware/build-windows-cardputer-adv` 或 `firmware/build-linux-cardputer-adv`，避免 Windows/WSL 共用 CMake 缓存。
`package-build` 按相同规则寻找固件，默认选择当前操作系统的 x64 工具包。

| 板型参数 | 芯片 | 接口 |
| --- | --- | --- |
| `cardputer-adv` | ESP32-S3 | 原生 USB，Cardputer UI |
| `esp32s3-uart0` | ESP32-S3 | USB-UART / UART0，无屏 |
| `esp32s3-native-usb` | ESP32-S3 | 原生 USB，无屏 |
| `esp32c5-uart0` | ESP32-C5 | USB-UART / UART0，无屏 |
| `esp32c5-native-usb` | ESP32-C5 | 原生 USB，无屏 |

当前无屏 S3 的 SDK默认配置为 16 MB，而主机板型定义仍为 8 MB；这些配置打包时会明确拒绝容量不一致，需先统一板型定义和 SDK 配置。工具不会跳过容量校验。

自定义构建目录时，在打包命令中传入匹配的 `--firmware-build`：

```sh
cargo run --locked -p wayline-tools -- firmware-build --board esp32c5-native-usb --output firmware/build-c5
cargo run --locked -p wayline-tools -- package-build --host-dir target/release --firmware-build firmware/build-c5 --board esp32c5-native-usb --output dist/wayline-c5 --version 0.1.0
```

`--board` 不与低层 `--target`、`--sdkconfig-defaults` 或 `--no-component-manager` 混用。
低层构建参数仍保留，便于专门的开发配置。若设置了 `CARGO_TARGET_DIR`，`--host-dir` 应指向实际的主机产物目录。

## 打包内容与校验

Windows 包包含桌面、桥接、服务、CLI、串口工具、固定版本 Wintun，以及所选板型固件和 Windows esptool。
Linux 包包含 CLI、串口工具、所选板型固件和 Linux esptool；不包含尚未移植的桌面、Windows 服务或 Wintun。
`--platform windows-x64|linux-x64` 可显式选择打包目标，但不执行交叉编译，输入主机二进制必须匹配目标架构。

Cardputer 包的目录结构：

```text
dist/wayline/
  wayline-desktop.exe       # Windows
  wayline-bridge.exe        # Windows
  wayline-service.exe       # Windows
  wayline-cli[.exe]
  wayline-radio[.exe]
  wintun.dll               # Windows
  firmware/
    manifest.json
    bootloader.bin
    partition-table.bin
    owl-radio.bin
  tools/esptool/
    esptool[.exe]
    LICENSE
    tool.json
    ...                    # 官方分发包其余文件
  licenses/
  package-info.json
  release.json
```

其他板型的固件放在 `firmware/profiles/<板型>/`。一次目录包选择一个板型。

打包器读取 ESP-IDF 的 `project_description.json`、`flasher_args.json`、`sdkconfig` 和镜像文件，生成长度、偏移与 SHA-256 清单，并调用运行时的 `wayline-flash` 校验器验证分区、芯片与容量。
固件版本和来源标签保留 CMake `project_version`，包括 `-dirty` 后缀；不会拿打包机器上的另一个 Git HEAD 代替固件来源。

烧录工具固定为项目要求的 esptool **5.0.2**，Windows Wintun 固定为 **0.14.1**。
下载先校验内置 SHA-256，再受限解包；校验失败、路径越界、链接、重复文件、超限数据或现有输出目录都会报错。
本机工具会执行 `esptool version` 验证；跨系统打包仅校验固定归档和可执行文件架构，在 `tool.json` 中记录 `nativeVersionChecked: false`。
所有文件写入临时目录，成功后才发布最终目录；失败不会留下被标为成功的目录包。

主机输入只选择必要程序，不复制 `target` 下的依赖、增量编译缓存或 PDB。输出是目录包，不是服务安装器；安装服务、刷写固件和实机互通验证仍是独立步骤。

## 缓存与离线输入

下载缓存在 `.cache/downloads`，可通过 `--cache-dir` 指定共享缓存。网络代理使用标准 `HTTP_PROXY` / `HTTPS_PROXY` 环境变量。
已损坏的缓存会报错，不会静默使用或跳过校验。

离线打包时提供官方归档原文件；解压后的目录不能替代归档校验：

```sh
cargo run --locked -p wayline-tools -- package-build --host-dir target/release --board cardputer-adv --output dist/wayline-offline --version 0.1.0 --esptool-archive downloads/esptool-v5.0.2-windows-amd64.zip --wintun-archive downloads/wintun-0.14.1.zip
```

Linux 使用 `esptool-v5.0.2-linux-amd64.tar.gz`，并省略 `--wintun-archive`；包组装逻辑和校验规则相同。

只生成固件包或烧录工具目录时也可单独调用：

```sh
cargo run --locked -p wayline-tools -- firmware-package --board cardputer-adv --output build/firmware-package
cargo run --locked -p wayline-tools -- package-verify build/firmware-package/manifest.json
cargo run --locked -p wayline-tools -- tool-package --output build/esptool
```

`firmware-package` 可用 `--build-dir` 指定已有 ESP-IDF 输出，`tool-package` 可用 `--archive` 指定离线归档。
原来的 `package-build --input DIR` 仍用于通用目录复制与哈希清单生成，不自动补齐固件或工具；完整包使用 `--host-dir` 模式。
