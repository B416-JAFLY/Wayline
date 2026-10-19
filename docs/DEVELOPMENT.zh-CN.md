# 开发指南

[English](DEVELOPMENT.md) · [项目说明](../README.zh-CN.md)

从仓库根目录的 PowerShell 执行命令。Windows Cargo workspace 包含 `apps/` 和 `crates/`；`firmware/` 使用独立的 Cargo manifest、锁文件和 Xtensa 配置。安装与日常使用见[快速开始](QUICKSTART.zh-CN.md)。

## 构建与测试

| 目标 | 依赖 |
| --- | --- |
| Windows 应用 | Git、Rust MSVC、Visual Studio C++ Build Tools、Windows SDK（含 `fxc.exe`） |
| OWL | PATH 中的 MinGW-w64 GCC 和 Git，以及 CMake、Ninja |
| 固件 | ESP-IDF 6.0，以及通过 `espup` 安装的 ESP32-S3 Xtensa 工具链 |

先构建 OWL，再构建主机应用：

```powershell
scripts/build-owl.ps1 -Test
scripts/build-release.ps1
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check -p omdrop-proto --no-default-features --locked
```

OWL 输出位于 `native/owl/build`，主机程序位于 `target/release`。OWL 脚本获取固定版本源码、应用[补丁](../native/owl/patches/README.md)并校验 Wintun。CMake/Ninja 从 PATH、CMake/ESP-IDF 安装目录（含 `IDF_TOOLS_PATH`）和 Visual Studio 中查找，可用 `-CMakePath`、`-NinjaPath` 覆盖。OWL 回归测试无需硬件。

仅开发桌面时运行 `scripts/build-desktop.ps1`，加 `-DebugBuild` 构建调试版。脚本查找 SDK 着色器编译器，可用 `GPUI_FXC_PATH` 覆盖。GPUI 的间接宏依赖使用本地 [stacksafe-macro 补丁](../vendor/stacksafe-macro/README.md)，需保留源码和许可证。

服务测试使用临时配置、独立管道和模拟 OWL 子进程：

```powershell
cargo test -p omdrop-service --all-targets --locked
```

以下忽略测试会操作已安装的服务，并在结束后让它保持停止。运行前先完成传输：

```powershell
cargo test -p omdrop-service installed_service_lifecycle_without_administrator_rights -- --ignored
```

主机测试不能替代固件构建、普通用户安装或设备测试。`just --list` 可查看任务快捷入口。

### 固件构建与刷写

```powershell
cargo install espup --locked
espup install --targets esp32s3 --std
scripts/build-firmware.ps1
cargo fmt --manifest-path firmware/Cargo.toml -- --check
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
cargo clippy --manifest-path firmware/Cargo.toml --all-targets --all-features --locked -- -D warnings
```

构建脚本支持已激活的 ESP-IDF 6.0 环境、EIM 安装目录或 `-IdfProfile <activation-script>`。输出为 `firmware/build-rust/owl-radio.bin`，配置保存在 `firmware/sdkconfig.defaults` 和 `sdkconfig.defaults.cardputer`。

刷写前退出桌面，停止占用端口的独立服务或监视器。在已激活的 ESP-IDF 终端中，将 COM4 换成实际设备端口：

```powershell
$fw = (Resolve-Path firmware).Path
idf.py -C $fw -B "$fw\build-rust" -D "SDKCONFIG=$fw\build-rust\sdkconfig" -p COM4 flash
python tests/firmware_regression.py -p COM4
```

刷写包含 bootloader、分区表和应用。若设备停在下载模式，松开 G0 后点按 RST。完成后重新打开桌面以恢复无线服务。

## 桌面应用

桌面与 CLI 共用 `omdrop-core` 收发逻辑、`omdrop-host` 设置与身份及 AWDL 选择、`omdrop-flash` 固件操作和 `omdrop-service` 管道客户端。普通权限 UI 与 LocalSystem 服务共用 `omdrop-desktop.exe`；必须在 GPUI、单实例处理和资源管理器注册前分派 `--service`。

启动时取得服务会话。托盘退出或 `Ctrl+Q` 取消桌面工作线程并释放会话，最后一个按需客户端等待 OWL 回收后才关闭服务。关闭卡片只隐藏界面，任务继续运行。开发者安装需先退出已安装的桌面，再在管理员终端运行 `scripts/install-service.ps1`；`-Build` 会先构建。以普通权限启动 UI，确保资源管理器拖放可用。

| 参数 | 行为 |
| --- | --- |
| `--send -- <paths...>` | 打开文件，或转发给运行中的实例 |
| `--show-in-taskbar` | 在任务栏和 Alt-Tab 显示卡片；托盘注册失败时保留可用窗口 |
| `--unregister-shell` | 退出后移除当前用户的文件右键和“发送到”入口 |
| `--version` | 输出版本，不初始化 GPUI 或服务 |

名称、接收目录和身份保存在 `%LOCALAPPDATA%\omdrop`。`desktop.json` 在进程锁下原子写入并保留未知设置；身份创建和 `receiver-services.json` 中的广播更名记录也受锁保护。`download_directory` 默认指向 Downloads/Omdrop，更改时不移动已有文件，接收期间禁止更改。完整接收错误写入 `desktop-receiver.log`。

接收须经用户确认，完成须通过归档校验和最终协议回应。固件维护会暂停空闲接收器；活动传输或其他接收器会阻止维护。取消或退出时，等待烧录子进程结束后再释放设备。

### 界面预览与资源

无需服务、资源管理器注册或硬件即可运行共用渲染器：

```powershell
cargo run -p omdrop-desktop --example orb_preview --locked
```

空格或预览按钮切换连接状态，Esc 关闭预览。在 `--` 后传入参数：`--borderless` 预览透明窗口，`--notice`、`--long-notice`、`--expanded-notice` 预览接收提示，`--long-content` 检查长内容滚动。加 `--check-layout` 执行单帧边界检查。

`orb_geometry.rs` 负责有界投影和连接过渡，`orb.rs` 在隐藏页面暂停动画。`window_frame.rs` 保留逐像素透明度，并排除透明角落的鼠标命中。托盘独立于卡片可见性更新无线状态，资源管理器重启后恢复图标。

[品牌母版](../assets/branding/README.md)通过 Node.js 和 `sharp` 生成纳入版本控制的资源，可用 `OMDROP_SHARP_MODULE` 指定已安装的包：

```powershell
node scripts/generate-desktop-icons.cjs
node scripts/generate-firmware-logo.cjs
```

普通构建直接使用已有资源。桌面嵌入 ICO/SVG，页眉按显示缩放栅格化 SVG；固件将有界 RLE Logo 直接解码到帧缓冲。

## CLI 与串口诊断

无线命令取得已安装的 `OmdropOwl` 会话，配置应用防火墙规则，并选择 `awdl0` 的链路本地地址。`status` 和 `doctor` 只读状态，不启动服务、创建身份文件或打开串口。

```powershell
cargo run -p omdrop-cli -- status --json
cargo run -p omdrop-cli -- doctor --json
cargo run -p omdrop-cli -- discover --timeout 15
cargo run -p omdrop-cli -- send --to "iPhone Name" --format auto -- "$env:USERPROFILE\Pictures\photo.jpg"
cargo run -p omdrop-cli -- receive
cargo run -p omdrop-cli -- receive --accept-all --save "$env:USERPROFILE\Downloads\Omdrop" --json
```

交互发送可按编号选择设备；脚本和 JSON 模式必须提供精确且无歧义的 `--to`。`--format auto` 优先使用对端声明的 DVZip，否则使用 gzip CPIO。`--no-ble` 关闭唤醒广播，`--name` 和 `--save` 仅覆盖本次命令的共用设置。

接收先显示发送方、文件和保存位置，再询问是否接受。EOF、取消或 `--decision-timeout` 超时（默认 60 秒）会拒绝请求。非交互与 JSON 接收必须显式传入 `--accept-all`，授权仅对本次命令有效。

`Ctrl+C` 取消任务、等待接收工作线程结束并发送 mDNS goodbye，服务丢失也会取消任务。一个发送与一个无线接收可以并行，同类第二个任务返回忙碌。归档在目标文件系统的临时目录中解包，通过校验后才发布，失败不留下可见的部分文件。

接口诊断需同时指定同一活动接口的 `--link-local ADDRESS --iface-index N`。`--bind` 则启动不使用无线会话和 mDNS 的本地 TCP 接收器：

```powershell
cargo run -p omdrop-cli -- receive --bind "[::1]:8771" --accept-all --save "$env:USERPROFILE\Downloads"
```

接收进度统计编码后的 HTTP 字节；上传成功须收到接收方最终回应。失败诊断包含归档/HTTP 阶段、字节数、内容类型和待完成的帧操作。

桌面与无线 CLI 启用 `ReceiverOptions::allow_dvzip_archive_boundary`，兼容省略 HTTP 最终零长度块的发送端。它仅适用于 chunked DVZip：CPIO 必须到达尾标记，当前 DVZip 块通过校验和与长度验证，HTTP 块以 CRLF 结束。缓冲中或尚未收全的 TLS/HTTP/DVZip 尾部仍须验证。完整归档边界没有待处理数据时，接收器回应并关闭连接，产生 `ArchiveBoundary`（JSON 为 `uploadArchiveBoundary`）。通用核心接收器、`--bind`、Content-Length 和 gzip 上传要求严格的 HTTP 完成边界。

### JSON 与退出码

`--json` 输出 JSON Lines，包含 `schemaVersion: 1`、`event` 和 `data`。进度与诊断之后恰有一个 `result` 事件，包含 `command`、`ok`、`exitCode`，失败时附带 `category` 和 `message`。人类可读诊断写入 stderr，JSON 命令不询问终端输入。

```json
{"schemaVersion":1,"event":"result","data":{"command":"discover","ok":true,"exitCode":0}}
```

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功，包括未发现匹配设备 |
| 2 | 参数无效或缺少无人值守授权 |
| 3 | 服务、设备或指定接收方不可用 |
| 4 | 接收方拒绝 |
| 5 | 操作、连接、文件或校验失败 |
| 6 | 资源忙碌 |
| 130 | 已取消 |

### 固件操作

`firmware info` 校验固件包并列出 USB 候选设备，不打开端口。烧录必须明确指定端口和板型；更新要求可识别的 Omdrop 固件且分区表一致，重装需同时提供两个替换参数：

```powershell
omdrop-cli firmware info --json
omdrop-cli firmware flash --port COM4 --board cardputer-adv
omdrop-cli firmware flash --port COM4 --board cardputer-adv --reinstall --yes-replace
```

`--manifest PATH` 和 `--tool PATH` 指定开发输入，仍校验工具版本。未安装服务时允许独立首次安装；已安装但不可用的服务会报错。两种模式都受设备锁保护。

### 串口诊断

退出桌面，停止占用端口的独立服务或监视器。将 COM4 换成实际端口；Python 诊断需要 `pyserial`：

```powershell
cargo run -p omdrop-radio -- COM4 hello
cargo run -p omdrop-radio -- COM4 stats
python tests/radio_probe.py -p COM4 hello
python tests/firmware_regression.py -p COM4
```

固件回归覆盖命令、损坏/超长帧恢复和重复 TIME 请求。`--tx-frames 20` 发送广播测试帧，`--soak-seconds 20` 检查持续通信。完成后重新打开桌面或启动服务。

## 服务配置与 IPC

配置位于 `%ProgramData%\omdrop-owl\config.json`，采用原子写入：

```json
{"port":"auto","channel":6,"enabled":true,"owlPath":null}
```

`port` 接受 `auto` 或 COM 端口，ESP32-S3 使用信道 6。`owlPath` 为绝对路径，null 表示服务旁的 `owl.exe`。日志为 `service.log`、`owl.log` 和 `owl-console.log`。

桌面安装使用 `--service --on-demand`；`scripts/install-service.ps1 -Runtime Rust` 安装自动启动、带 `--resident` 的独立服务。两个服务入口均支持这两种生命周期模式，默认常驻。`-Uninstall` 移除服务注册，保留安装文件。

开发时创建独立数据目录和配置，再运行管理员控制台：

```powershell
cargo run -p omdrop-service -- --console --data-dir E:\path\to\service-data --pipe omdrop-dev
```

同一无线设备只能由一个管理进程持有。Ctrl+C 等待 OWL 退出最多五秒，超时后终止并回收子进程；不改变配置的 `enabled` 值。

默认控制管道为 `\\.\pipe\omdrop-owl`，请求是含 `cmd` 的 JSON 行：

| 命令 | 字段 | 效果 |
| --- | --- | --- |
| `status` | — | 读取无线状态 |
| `start`、`stop` | — | 持久化启用状态并启动/停止 OWL |
| `set` | 可选 `port`、`channel` | 更新无线配置 |
| `firewall` | `exe` | 为已存在的桌面或 CLI 程序配置规则 |

响应包含 `state`、`port`、`channel`、`awdl0`、`error` 和 `enabled`；停止时在子进程退出前返回 `stopping`。管道允许本地已认证用户连接，请求上限 8192 字节，未完整请求五秒超时。防火墙允许来自 `fe80::/10` 的 TCP 8771 和 UDP 5353 入站流量，仅作用于桌面/CLI 程序。

使用 `omdrop_service::client::Client` 配置管道、超时并取得类型化错误。控制命令 `stop` 持久禁用无线，会话释放和维护不会。

带版本的 `<control-pipe>.sessions` 管道持有客户端、发送、接收和维护租约。服务校验进程 ID，将令牌绑定到服务实例并回收断开的客户端。缺少能力时需更新服务，最终关闭期间拒绝新租约。

维护拒绝活动传输，回收 OWL 并阻止新任务，不改变 `enabled`。OWL 启动和烧录共享按端口命名的互斥锁，Job Object 确保设备复用前烧录工具已退出。写入中断会保留就绪失败记录，重试通过镜像校验和 HELLO 后才清除。

就绪记录为 Windows Public 已知文件夹下 `Omdrop/device-state` 中的 `COMx.failed`。任何失败记录都会阻止 `port: "auto"`。设备重新枚举可能改变端口号，成功重试只清除新端口记录。通过 `set` 明确配置已验证的新端口，或核实旧设备后处理其记录；不要清除无关失败。系统不会自动关联新旧 USB 身份。

## 离线发布包

从同一检出版本构建主机与固件，再使用新的输出目录。构建和打包不会启动服务、打开端口或刷写设备。

```powershell
scripts/build-owl.ps1 -Test
scripts/build-release.ps1
scripts/build-firmware.ps1
scripts/package-firmware.ps1 -OutputDirectory build/firmware-package
scripts/fetch-flash-tool.ps1 -OutputDirectory .cache/esptool-5.0.2-release
scripts/package-release.ps1 -OutputDirectory build/release-package -FirmwarePackage build/firmware-package -ToolDirectory .cache/esptool-5.0.2-release/esptool-windows-amd64
scripts/install-prebuilt.ps1 -PackageRoot build/release-package -VerifyOnly
```

主机构建记录为 `target/release/omdrop-build.json`，固件记录为 `firmware/build-rust/omdrop-build.json`。打包校验源码版本、程序版本、哈希、OWL 固定版本/补丁、镜像范围和工具元数据。固件 `manifest.json` 描述板型、分区布局、协议和镜像，`release.json` 记录包内每个文件的长度与 SHA-256；缺失、修改或未列出的文件都会使校验失败。

[下载脚本](../scripts/fetch-flash-tool.ps1)固定并校验独立 esptool 5.0.2，使用其输出的工具目录。烧录在取得维护前校验输入，使用 ROM `--no-stub` 命令，逐镜像验证后复位并检查 HELLO。更新保留未写入的 NVS，写入中断可能需要完整重试。

| 打包参数 | 内容 |
| --- | --- |
| 默认 | 桌面、服务、OWL/Wintun、固件和烧录工具 |
| `-IncludeCli` | 向运行时包加入 CLI |
| `-Runtime Rust -IncludeCli` | 常驻服务与 CLI，不包含固件和烧录工具 |
| `-Runtime Cli` | 不含安装器或服务的 CLI 工具包 |

独立 CLI 工具包使用已安装的服务，固件命令需显式提供 `--manifest` 和 `--tool` 路径。包内包含依赖清单、适用许可证和应用本地 x64 Visual C++ 运行库。打包器通过 `vswhere` 查找运行库，可用 `-VCRuntimeDirectory` 和 `-VCRuntimeNotice` 覆盖。

`install.cmd` 先校验包，再替换 `%ProgramFiles%\omdrop-owl` 下的程序；保留旧目录备份，失败时恢复文件和服务配置。安装前退出桌面与 CLI。以普通用户启动安装器，仅服务注册提升权限，UI 以普通权限启动；已提升权限的安装器不会启动 UI。配置和下载文件保存在程序目录之外。`scripts/install-service.ps1 -PackageRoot <directory>` 调用同一安装器。

无需修改已安装服务即可运行安装器与包校验：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File tests/release_validation.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File tests/installer_service_configuration.ps1
```

分发前，在干净 Windows x64 和 Cardputer Adv 实机上验证离线安装、首次刷写、更新、中断恢复与 Apple 设备收发。

## 固件与界面

Rust 负责协议分帧、RX/TX、过滤、计数器、对端跟踪、界面与翻译。`firmware/components/radio/radio_hal.c` 封装 ESP-IDF，`radio_ui.cpp` 封装 M5Unified 显示、键盘、音频、背光和 NVS。共享 C 定义位于 `crates/omdrop-proto/include/proto.h`，主机与固件布局必须一致。

固件不使用动态内存分配。短状态访问使用自旋锁，无线配置和 USB 输出使用独立互斥锁；阻塞驱动调用必须在状态自旋锁之外。界面采用静态 240×135 RGB565 帧缓冲，`ui_orb.rs` 使用有界定点几何。隐藏页面、传输卡片、熄屏或关闭动效时暂停动画。板载操作见[快速开始](QUICKSTART.zh-CN.md#板载界面)。

### 预览与翻译

安装 Pillow 后，用板端同一 Rust 渲染器导出预览：

```powershell
cargo run --manifest-path firmware/Cargo.toml --features cardputer-ui --example ui_preview -- build/cardputer-ui
python scripts/render-ui-preview.py
```

`build/cardputer-ui/` 包含三种语言的页面、传输卡片、声音设置和动画。导出帧后重新生成 README 动图：

```powershell
python scripts/render-ui-preview.py --readme
python scripts/render-airdrop-demo.py
python scripts/render-airdrop-demo.py --direction pc-to-iphone
```

输出为 `docs/assets/cardputer-demo.gif`、`airdrop-esp32-omdrop.gif` 和 `omdrop-esp32-airdrop.gif`。导出器检查尺寸、时长与传输状态顺序，联系图和元数据保留在 `build/cardputer-ui/`。收发演示采用示意手机/桌面面板和实际固件渲染，合成器自动查找 CJK 字体或接受 `--font <font-file>`。预览不验证无线和硬件行为。

修改 `firmware/assets/locales.json` 后，一起重新生成字符串与字体子集。需要 Pillow、fontTools 和 `firmware/assets/font-source.txt` 指定的字体：

```powershell
python scripts/generate-ui-font.py path/to/NotoSansCJKsc-Regular.otf
cargo fmt --manifest-path firmware/Cargo.toml
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
```

提交生成的子集和 SIL OFL 许可证，普通构建使用这些已有文件。

## 板端传输反馈

桌面和 CLI 调用 `omdrop_core::enable_board_feedback()`。`omdrop-board` 通过 OWL 的 `omdrop-radio-ui` 管道每秒最多发布五次更新，并每秒发送心跳。反馈失败不影响文件收发，也不代表授权接受请求。

发送与接收使用独立状态通道。新任务唤醒屏幕并暂停自动熄屏；成功卡片十秒消失，失败/取消卡片三十秒消失，八秒无心跳将活动反馈标记为丢失。Windows 栅格化 Unicode 设备名称字形。

协议 v2 的 `0x08` 携带 `crates/omdrop-proto/src/transfer.rs` 中定义的 1,152 字节帧。`0x09` 查询方向 0/1，通过 `0x89` 返回 32 字节头。管道接受 `0 + frame` 或 `1 + direction`，返回状态 0 和查询数据（拒绝为 1），再等待 `0xff`。发布仅确认 OWL 入队，查询确认固件状态。

OWL 与固件运行时，验证文件内容、状态通道、拒绝与取消：

```powershell
cargo run -p omdrop-core --example board_loopback
cargo run -p omdrop-core --example board_loopback -- --failure-cases
```

## 数据通路与基准测试

### 实验性接收聚合

可选 A-MPDU 接收支持 Apple → ESP32 → PC。固件处理 ADDBA/DELBA 与硬件 Block Ack，OWL 不需要额外参数。普通构建默认关闭，尚未实现 PC → Apple 发送聚合。吞吐与 BA 时序需要实机验证。

```powershell
scripts/build-firmware.ps1 -RxAggregation
```

输出和构建记录位于 `firmware/build-rust-aggregation/`，附加配置为 `sdkconfig.defaults.aggregation`；普通打包使用 `firmware/build-rust/`。释放串口后，在已激活的 ESP-IDF 环境刷写：

```powershell
$fw = (Resolve-Path firmware).Path
idf.py -C $fw -B "$fw\build-rust-aggregation" -D "SDKCONFIG=$fw\build-rust-aggregation\sdkconfig" -p COM4 flash
```

将 COM4 换成实际端口，完成后重新打开桌面。对比时固定板型、Apple 设备、信道、文件和摆放位置，检查文件哈希、取消、重复传输、断开重连与丢包恢复。独立抓包须确认 ADDBA、聚合 MPDU 和 BA，回应入队不等于无线协商成功。

查询计数器前释放串口，计数器保留到设备复位：

```powershell
python tests/radio_probe.py -p COM4 aggregation
python -m unittest discover -s tests -p test_radio_aggregation.py
```

`ampdu_mpdu` 统计接纳的聚合 MPDU，`ampdu_rx_drop` 统计上行丢帧，`accepted` 统计 ADDBA 回应入队。`response_errors`、`control_drop` 和 `tx_report_drop` 显示队列/驱动失败。`MSG_STATS` 请求体 `BA` 返回 56 字节 v1 响应，字段见 [proto.h](../crates/omdrop-proto/include/proto.h)。

策略只允许一个对端/TID、2–16 个 MPDU、立即 BA，且不允许 A-MPDU 内含 A-MSDU。重复请求保留硬件位图，建立失败不留下新会话。DELBA、空闲超时、主机丢失或无线更改会拆除会话；代次检查使过期请求失效，有界控制/上行队列保证 Wi-Fi 回调不阻塞。

C HAL 在未关联 STA 上使用接收 BA 表槽位 0，不兼容 AP/SoftAP。ESP-IDF 内部 BA API 要求 SDK 归档哈希匹配[无线 CMake 配置](../firmware/components/radio/CMakeLists.txt)。调用位于配置互斥锁下、状态自旋锁外；表项回读仅确认编程结果，默认启用前仍需验证无线时序。

### 流式处理与编解码基准

固件使用静态 4096 字节缓冲批量发送已排队的 RX 与 TX 完成消息，每批最多八帧，各自保留协议 v2 CRC/COBS 分隔。USB 写入前归还环形队列项，刷新期间最多持有一个待处理项。命令使用独立编码器，仅在完整写入期间持有 TX 锁；空队列不等待凑批。RX 环形队列为 48 KiB，USB TX/RX 缓冲各为 8 KiB。

双核原生 USB 构建将驱动初始化、对应中断和上行任务放在 Wi-Fi 任务的另一核心，卸载驱动须使用安装核心。UART/单核构建使用常规初始化，共享 CRC/COBS 编解码器以 `opt-level = 3` 编译。

释放串口后运行：

```powershell
python tests/radio_probe.py -p COM4 uplink
```

`MSG_STATS` 请求体 `UP` 返回十四个 u32le 字段（56 字节，v1），包含批量统计、驱动字节数、编码/写入/互斥锁计时、错误、最大值、堆与中断核心，见 [proto.h](../crates/omdrop-proto/include/proto.h)。总量按 u32 回绕，评估时需同时记录丢帧、传输耗时和文件哈希。

共享编解码器在一次 CRC/COBS 遍历中处理分段输入。OWL 同样按最多 4096 字节/八帧批量写入，保持 FIFO、部分写入与背压处理。接收将 DVZip 流式解码到有界 CPIO 解包器，发送将 CPIO 流式压缩为 gzip/DVZip 临时归档。成功须经过边界校验和接收端最终确认。

```powershell
cargo run --release -p omdrop-proto --example codec_bench
cargo run --release -p omdrop-airdrop --example archive_bench
cargo run --release -p omdrop-airdrop --example archive_bench --features flate2/zlib-rs
native/owl/build/owl_test_radio_io.exe --bench
cargo test -p omdrop-airdrop -p omdrop-core --features flate2/zlib-rs
```

主机基准测量编解码 CPU 成本。无线吞吐须在固定板型、信道和文件的实机环境测量，同时记录耗时、完整性和丢帧计数。
