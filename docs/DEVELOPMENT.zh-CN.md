# 开发指南

[English](DEVELOPMENT.md) · [项目说明](../README.zh-CN.md)

从仓库根目录执行命令。Windows Cargo workspace 包含 `apps/` 和 `crates/`；`firmware/` 使用独立的 Cargo manifest、锁文件和 Xtensa 配置。安装与日常使用见[快速开始](QUICKSTART.zh-CN.md)。

## 构建与测试

| 目标 | 依赖 |
| --- | --- |
| Windows 应用 | Git、Rust MSVC、Visual Studio C++ Build Tools、Windows SDK（含 `fxc.exe`） |
| 固件 | ESP-IDF 6.0，以及通过 `espup` 安装的 ESP32-S3 Xtensa 工具链；ESP32-C5 构建另需 RISC-V 目标 |

构建并测试可移植的主机应用：

```sh
cargo run --locked -p wayline-tools -- host-build --test
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check -p wayline-proto --no-default-features --locked
```

如果只需进行不依赖 Wintun、Windows SDK 或服务安装的主机检查，运行
`cargo run --locked -p wayline-tools -- host-build --test`。非 Windows 系统会构建并测试 CLI 和共用主机 crate；
Windows 还会加入桥接程序、桌面和服务 crate。加 `--release` 可使用 release profile。
串口诊断在所有平台都必须显式传入 `--port`，或设置 `WAYLINE_SERIAL_PORT`。

固件构建使用 Rust 包装器，并要求已激活 ESP-IDF 环境：

```sh
cargo run --locked -p wayline-tools -- firmware-build --target esp32s3 --output firmware/build-rust --sdkconfig-defaults "sdkconfig.defaults;sdkconfig.defaults.cardputer"
```

Windows 发布打包流程会在 `target/release` 中加入 Rust 桥接程序和校验后的 Wintun。

仅开发桌面时运行 `cargo build --locked -p wayline-desktop`，发布构建加 `--release`。GPUI 着色器编译器不在工具链路径时设置 `GPUI_FXC_PATH`。GPUI 的间接宏依赖使用本地 [stacksafe-macro 补丁](../vendor/stacksafe-macro/README.md)，需保留源码和许可证。

服务测试使用临时配置、独立管道和模拟 Rust 桥接程序 子进程：

```sh
cargo test -p wayline-service --all-targets --locked
```

以下忽略测试会操作已安装的服务，并在结束后让它保持停止。运行前先完成传输：

```sh
cargo test -p wayline-service installed_service_lifecycle_without_administrator_rights -- --ignored
```

主机测试不能替代固件构建、普通用户安装或设备测试。`just --list` 可查看任务快捷入口。

### Rust AWDL 后端

`apps/bridge` 是服务管理的无线子进程；`crates/wayline-awdl` 实现 ESP32 串口、
Wintun、IPv6 和板端反馈适配。AWDL 协议来自固定的 Owly 0.4.0，
[源码与本地适配说明](../vendor/owly-awdl/README.md)记录版本和许可证。
AirDrop 协议和状态机统一使用固定版本的 `owly-airdrop`；`wayline-core` 负责 Windows 收发运行、回调、取消、身份存储与板端反馈。固件协议和桌面交互保持不变。

```sh
cargo test --locked -p wayline-awdl -p wayline-bridge --all-targets
cargo test --locked --manifest-path vendor/owly-awdl/Cargo.toml --all-features
```

输出是 `target/release/wayline-bridge.exe`（调试版在 `target/debug`），旁边放置
经过固定版本 SHA-256 校验的 `wintun.dll`、`bridge-licenses` 和来源记录
`wayline-bridge-build.json`。构建不打开串口、不创建网卡、不修改已安装服务。
服务只运行 Rust 桥接程序。配置 `%ProgramData%\wayline\config.json` 的可选绝对
`bridgePath` 可覆盖服务程序旁的 `wayline-bridge.exe`；安装器在停止服务前检查其可用性。
构建本身不会更新已安装服务，设备互通仍待实机验收。
独立前台运行需要管理员终端、真实端口以及已退出的桌面/服务；`COM4` 仅为示例：

```sh
target/release/wayline-bridge.exe --port COM4 --channel 6
```

前台运行取得设备互斥锁，Ctrl+C 等待串口与 Wintun 线程退出；`--managed` 仅用于
服务启动，要求服务停止事件。板端反馈仍使用 `wayline-radio-ui`，反馈失败独立于
文件传输。服务会话、停止和异常回收沿用现有行为。

主机回归检查：

```sh
cargo test -p wayline-awdl -p wayline-bridge -p wayline-service -p wayline-core --all-targets --locked
cargo test --manifest-path vendor/owly-airdrop/Cargo.toml --all-features --lib --test vectors --locked
cargo test --manifest-path vendor/owly-awdl/Cargo.toml --all-features --locked
cargo clippy --manifest-path vendor/owly-awdl/Cargo.toml --all-targets --all-features --locked -- -D warnings
```

模拟串口、真实测试管道与模拟无线子进程不操作设备；真实 Wintun 驱动、ESP32
时序和 Apple 设备双向互通仍需设备验收，主机测试不能替代硬件验收。

### 固件构建与刷写

```sh
cargo install espup --locked
espup install --targets esp32s3 --std
cargo test --locked --manifest-path firmware/Cargo.toml --all-features
cargo fmt --manifest-path firmware/Cargo.toml -- --check
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
cargo clippy --manifest-path firmware/Cargo.toml --all-targets --all-features --locked -- -D warnings
```

ESP-IDF 构建需要已激活的 ESP-IDF 6.0 环境，输出为 `firmware/build-rust/owl-radio.bin`，配置保存在 `firmware/sdkconfig.defaults` 和 `firmware/sdkconfig.defaults.cardputer`。

刷写前退出桌面，停止占用端口的独立服务或监视器。在已激活的 ESP-IDF 终端中，将 COM4 换成实际设备端口：

```sh
$fw = (Resolve-Path firmware).Path
idf.py -C $fw -B "$fw\build-rust" -D "SDKCONFIG=$fw\build-rust\sdkconfig" -p COM4 flash
cargo run --locked -p wayline-radio -- COM4 hello
```

刷写包含 bootloader、分区表和应用。若设备停在下载模式，松开 G0 后点按 RST。完成后重新打开桌面以恢复无线服务。

### 无屏幕 ESP32-C5 构建

ESP32-C5 构建为无屏幕配置：不包含 M5Unified 和 Cardputer 界面，使用 RISC-V 目标，并输出到独立目录。

```sh
espup install --targets esp32s3,esp32c5
idf.py -C firmware -B firmware/build-rust-esp32c5 -D IDF_TARGET=esp32c5 build
idf.py -C firmware -B firmware/build-rust-esp32c5-usb -D IDF_TARGET=esp32c5 build
idf.py -C firmware -B firmware/build-rust-esp32s3-headless -D IDF_TARGET=esp32s3 build
idf.py -C firmware -B firmware/build-rust-esp32s3-headless-usb -D IDF_TARGET=esp32s3 build
```

默认 C5 镜像使用 USB-UART 桥接 UART0，输出到 `firmware/build-rust-esp32c5/`；加 `-C5NativeUsb` 构建原生 USB Serial/JTAG profile，输出到 `firmware/build-rust-esp32c5-usb/`。`-S3Headless` 构建关闭 M5Unified/Cardputer UI，输出 UART0 profile；加 `-S3NativeUsb` 构建无屏原生 USB profile。无屏 S3 profile 的镜像头、清单和分区布局统一为 8 MiB，可安装到 8 MiB 或 16 MiB S3，额外 Flash 容量不使用；4 MiB S3 设备不匹配此镜像。所有无屏构建关闭组件管理，ESP32-S3 的 `firmware/dependencies.lock` 保持不变。Desktop 离线包包含 Cardputer、无屏 S3 UART0/Native USB、C5 UART0/Native USB 五个 profile，设备固件页会按端口 USB 接口类型筛选。

```sh
$fw = (Resolve-Path firmware).Path
$out = Join-Path $fw 'build-rust-esp32c5'
idf.py -C $fw -B $out -D IDF_TARGET=esp32c5 -D "SDKCONFIG=$out\sdkconfig" -p COM4 flash
cargo run --locked -p wayline-radio -- COM4 hello
```

镜像启动时使用 2.4 GHz 信道 6；主机在 `%ProgramData%\wayline\config.json` 中把 `channel` 改为 44 或 149 后切换到 5 GHz，修改后重启服务。ESP-IDF 6.0 要求在启动 Wi-Fi 前固定原始发送速率，因此 C5 构建固定 HE20/MCS0 并忽略逐帧速率提示。AirDrop 互通与吞吐仍需实机验证。

## 桌面应用

桌面与 CLI 共用 `wayline-core` 收发逻辑、`wayline-host` 设置与身份及 AWDL 选择、`wayline-flash` 固件操作和 `wayline-service` 管道客户端。普通权限 UI 与 LocalSystem 服务共用 `wayline-desktop.exe`；必须在 GPUI、单实例处理和资源管理器注册前分派 `--service`。

启动时取得服务会话。托盘退出或 `Ctrl+Q` 取消桌面工作线程并释放会话，最后一个按需客户端等待 Rust 桥接程序回收后才关闭服务。关闭卡片只隐藏界面，任务继续运行。服务安装和 ACL 管理在 Rust 安装器迁移完成前需要手动执行。以普通权限启动 UI，确保资源管理器拖放可用。

| 参数 | 行为 |
| --- | --- |
| `--send -- <paths...>` | 打开文件，或转发给运行中的实例 |
| `--show-in-taskbar` | 在任务栏和 Alt-Tab 显示卡片；托盘注册失败时保留可用窗口 |
| `--unregister-shell` | 退出后移除当前用户的文件右键和“发送到”入口 |
| `--version` | 输出版本，不初始化 GPUI 或服务 |

名称、接收目录、界面语言和身份保存在 `%LOCALAPPDATA%\wayline`。`desktop.json` 在进程锁下原子写入并保留未知设置；身份创建和 `receiver-services.json` 中的广播更名记录也受锁保护。`download_directory` 默认指向 Downloads/Wayline，更改时不移动已有文件，接收期间禁止更改。`language` 保存 `zh-CN`、`zh-TW`、`en-US`、`ja-JP`、`ko-KR`、`fr-FR`、`de-DE` 或 `es-ES`；缺少该字段时按 Windows 界面语言选择，不支持的系统语言回退到 English。`desktop_notifications_enabled` 控制是否显示独立桌面接收通知，默认开启；`notification_position` 支持左上、上中、右上、左下、下中、右下，默认 `top-right`；`notification_duration_seconds` 允许 3、5、8、10、15 或 30 秒，默认 8 秒。通知使用淡入淡出过渡。切换语言会立即更新窗口、托盘、系统对话框和通知，并重新注册资源管理器入口。界面文案位于 `apps/desktop/locales/*.toml`（当前 8 种语言），`apps/desktop/build.rs` 在构建时校验所有语言的键集合与占位符并生成静态文案表；新增语言时需要同时添加 TOML、`Language` 枚举项和 `build.rs` 构建列表，并保持全部翻译完整。共享模块（传输、烧录和本地设置校验）的底层错误详情仍按原文显示。完整接收错误写入 `desktop-receiver.log`。

`pinned` 默认 `false`，开启后分享卡片失去焦点时仍保持显示。`drag_activate` 和 `auto_open_download_dir` 默认 `true`，分别控制拖动时显示投放入口、确认接收成功后打开第一个文件的位置。缺失或 `null` 使用默认值，其他非布尔值报错；读取失败会在卡片中提示，保存失败保留内存中的原值。

活动发送与接收卡片按进度事件显示平滑速度，进度停滞一秒后速度归零。耗时从首个 `Uploading` 或 `Receiving` 事件（包括零字节事件）开始计算，持续到最终确认，排除准备和等待接受的时间。历史平均速度使用传输流字节数除以这一耗时。分享卡片顶部提供设备固件和最近传输图标入口。

页面、列表和长文本使用共用滚动区域。内容超出可视区时显示常驻比例滚动条，支持滚轮、拖动滑块和点击轨道。嵌套区域优先滚动内部内容，到达边界后再滚动外层。底部操作保持固定，包括历史删除确认的确认与取消按钮。

最多 200 条传输记录单独保存在 `%LOCALAPPDATA%\wayline\transfer-history.json`，使用进程锁和原子替换；完整历史 JSON 总大小上限为 4 MiB，超出数量或容量限制时淘汰最旧的完整记录，单条记录超过 4 MiB 时明确报错。成功只在最终协议确认后记入。记录包含原始文件字节数、传输流字节数、文件名、发送源路径和成功接收的文件路径，不保存文件内容。每个已记录文件均可打开所在位置；缺少文件明细的旧版 version-1 记录仍可读取。

历史列表使用三行紧凑布局，依次显示设备与结果、文件概览与大小、时间与操作。传输量、耗时、平均速度、完整文件清单、文件位置、无线频段、信道、传输模式和失败原因在“详情”中显示；旧记录可能没有无线信息。

删除单条记录或清空记录时始终显示确认，提供“同时删除文件”和“记住此选项”，初始均不勾选。记住选项仅更新文件删除默认值，保存在 `desktop.json` 的 `history_delete_files` 字段中，缺省为 `false`；设置页可修改同一默认值，每次删除仍需确认。同时删除会永久删除记录中明确保存的普通文件（包括发送源文件），不递归删除保存目录，也不跟随符号链接或 Windows 重解析点。文件已不存在视为已删除；其他删除错误会保留相关记录以便重试。

传输文件夹时保留顶层名称与路径，供显示和打开所在位置。勾选“同时删除文件”后，如果所选记录中包含文件夹，预检会拒绝整批删除并保留所有记录，避免误删传输后加入文件夹的新文件。请打开文件夹所在位置处理其内容，或取消勾选，仅删除历史记录。

接收须经用户确认，完成须通过归档校验和最终协议回应。固件维护会暂停空闲接收器；活动传输或其他接收器会阻止维护。取消或退出时，等待烧录子进程结束后再释放设备。

### 界面预览与资源

无需服务、资源管理器注册或硬件即可运行共用渲染器：

```sh
cargo run -p wayline-desktop --example orb_preview --locked
```

空格或预览按钮切换连接状态，Esc 关闭预览。在 `--` 后传入参数：`--borderless` 预览透明窗口，`--notice`、`--long-notice`、`--expanded-notice` 预览接收提示，`--long-content` 检查长内容滚动。加 `--check-layout` 执行单帧边界检查。

无需硬件即可预览共用滚动组件：

```sh
cargo run -p wayline-desktop --example scroll_preview --locked
```

按钮或快捷键 1/2/3 分别选择长列表、短列表和嵌套内容，Esc 关闭预览。可检查溢出时的滚动条显示、滚轮、滑块拖动、轨道点击和底部固定操作。

`orb_geometry.rs` 负责有界投影和连接过渡，`orb.rs` 在隐藏页面暂停动画。`window_frame.rs` 保留逐像素透明度，并排除透明角落的鼠标命中。托盘独立于卡片可见性更新无线状态，资源管理器重启后恢复图标。

[品牌母版](../assets/branding/README.md)通过 Node.js 和 `sharp` 生成纳入版本控制的资源，可用 `WAYLINE_SHARP_MODULE` 指定已安装的包：

```sh
node scripts/assets/generate-desktop-icons.cjs
node scripts/assets/generate-firmware-logo.cjs
```

普通构建直接使用已有资源。桌面嵌入 ICO/SVG，页眉按显示缩放栅格化 SVG；固件将有界 RLE Logo 直接解码到帧缓冲。

## CLI 与串口诊断

无线命令取得已安装的 `Wayline` 会话，配置应用防火墙规则，并选择 `awdl0` 的链路本地地址。`status` 和 `doctor` 只读状态，不启动服务、创建身份文件或打开串口。

```sh
cargo run -p wayline-cli -- status --json
cargo run -p wayline-cli -- doctor --json
cargo run -p wayline-cli -- radio status --json
cargo run -p wayline-cli -- radio set --band 5ghz --channel 44 --json
cargo run -p wayline-cli -- discover --timeout 15
cargo run -p wayline-cli -- send --to "iPhone Name" --format auto -- "$env:USERPROFILE\Pictures\photo.jpg"
cargo run -p wayline-cli -- receive
cargo run -p wayline-cli -- receive --accept-all --save "$env:USERPROFILE\Downloads\Wayline" --json
```

交互发送可按编号选择设备；脚本和 JSON 模式必须提供精确且无歧义的 `--to`。`--format auto` 优先使用对端声明的 DVZip，否则使用 gzip CPIO。`--no-ble` 关闭唤醒广播，`--name` 和 `--save` 仅覆盖本次命令的共用设置。

`radio status` 读取服务报告的板型、配置频段和信道；`radio set` 会启动服务会话并等待 bridge 重启完成。频段与信道必须匹配：ESP32-S3 仅支持 2.4 GHz 信道 6，ESP32-C5 支持 2.4 GHz 信道 6 和 5 GHz 信道 44、149。桌面设置页使用同一能力状态。活动发送、接收或固件维护期间，service 拒绝无线配置变更；重启期间也暂拒新的收发和维护会话。更换无线端口前须先切回信道 6，再由新板型确认 5 GHz 能力。若在 ESP32-S3 上遗留 5 GHz 配置，可从桌面选择信道 6，或运行 `radio set --band 2.4ghz --channel 6` 恢复。切换会重启 AWDL/Wintun，真实设备上的互通和吞吐仍需验收。

发现会在 `--timeout` 期间持续重试 mDNS 设备的 TCP/TLS `/Discover` 探测；连接失败后按最多 3 秒的退避间隔重试，以覆盖无线邻居或接收服务稍晚就绪的情况。

接收先显示发送方、文件和保存位置，再询问是否接受。EOF、取消或 `--decision-timeout` 超时（默认 60 秒）会拒绝请求。非交互与 JSON 接收必须显式传入 `--accept-all`，授权仅对本次命令有效。

`Ctrl+C` 取消任务、等待接收工作线程结束并发送 mDNS goodbye，服务丢失也会取消任务。一个发送与一个无线接收可以并行，同类第二个任务返回忙碌。归档在目标文件系统的临时目录中解包，通过校验后才发布，失败不留下可见的部分文件。

接口诊断需同时指定同一活动接口的 `--link-local ADDRESS --iface-index N`。`--bind` 则启动不使用无线会话和 mDNS 的本地 TCP 接收器：

```sh
cargo run -p wayline-cli -- receive --bind "[::1]:8771" --accept-all --save "$env:USERPROFILE\Downloads"
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

`firmware info` 校验固件包并列出 USB 候选设备，不打开端口。烧录必须明确指定端口和板型；更新要求可识别的 Wayline 固件且分区表一致，重装需同时提供两个替换参数：

```sh
wayline-cli firmware info --json
wayline-cli firmware flash --port COM4 --board cardputer-adv
wayline-cli firmware flash --port COM4 --board cardputer-adv --reinstall --yes-replace
wayline-cli firmware flash --port COM5 --board esp32s3-uart0 --reinstall --yes-replace
wayline-cli firmware flash --port COM6 --board esp32s3-native-usb --reinstall --yes-replace
wayline-cli firmware flash --port COM8 --board esp32c5-uart0 --reinstall --yes-replace
wayline-cli firmware flash --port COM8 --board esp32c5-native-usb --reinstall --yes-replace
```

`--board` 固定可选 profile 为 `cardputer-adv`、`esp32s3-uart0`、`esp32s3-native-usb`、`esp32c5-uart0` 和 `esp32c5-native-usb`，并与固件清单中的板型、芯片、Flash 容量及接口类型匹配。`--manifest PATH` 和 `--tool PATH` 指定开发输入，仍校验工具版本。未安装服务时允许独立首次安装；已安装但不可用的服务会报错。两种模式都受设备锁保护。

### 串口诊断

退出桌面，停止占用端口的独立服务或监视器。将 COM4 换成实际端口：

```sh
cargo run --locked -p wayline-radio -- COM4 hello
cargo run --locked -p wayline-radio -- COM4 stats
cargo run --locked -p wayline-radio -- COM4 time
cargo run --locked -p wayline-radio -- COM4 channel 6
cargo run --locked -p wayline-radio -- COM4 filter awdl
```

协议分帧和编解码回归通过 `cargo test -p wayline-proto` 与 `cargo test -p wayline-radio` 执行。完成后重新打开桌面或启动服务。

## 服务配置与 IPC

配置位于 `%ProgramData%\wayline\config.json`，采用原子写入：

```json
{"port":"auto","channel":6,"enabled":true,"bridgePath":null}
```

`port` 接受 `auto` 或 COM 端口，ESP32-S3 使用信道 6；ESP32-C5 还可使用信道 44 和 149。`bridgePath` 为绝对路径，null 表示服务旁的 `wayline-bridge.exe`。日志为 `service.log` 和 `bridge-console.log`。

桌面安装使用 `--service --on-demand`；服务安装和卸载暂由 Windows 管理工具手动完成。

开发时创建独立数据目录和配置，再运行管理员控制台：

```sh
cargo run -p wayline-service -- --console --data-dir E:\path\to\service-data --pipe wayline-dev
```

同一无线设备只能由一个管理进程持有。Ctrl+C 等待 Rust 桥接程序 退出最多五秒，超时后终止并回收子进程；不改变配置的 `enabled` 值。

默认控制管道为 `\\.\pipe\wayline`，请求是含 `cmd` 的 JSON 行：

| 命令 | 字段 | 效果 |
| --- | --- | --- |
| `status` | — | 读取无线状态 |
| `start`、`stop` | — | 持久化启用状态并启动/停止 Rust 桥接程序 |
| `set` | 可选 `port`、`channel` | 更新无线配置 |
| `firewall` | `exe` | 为已存在的桌面或 CLI 程序配置规则 |

响应包含 `state`、`port`、`channel`、`awdl0`、`error` 和 `enabled`；停止时在子进程退出前返回 `stopping`。管道允许本地已认证用户连接，请求上限 8192 字节，未完整请求五秒超时。防火墙允许来自 `fe80::/10` 的 TCP 8771 和 UDP 5353 入站流量，仅作用于桌面/CLI 程序。

使用 `wayline_service::client::Client` 配置管道、超时并取得类型化错误。控制命令 `stop` 持久禁用无线，会话释放和维护不会。

带版本的 `<control-pipe>.sessions` 管道持有客户端、发送、接收和维护租约。服务校验进程 ID，将令牌绑定到服务实例并回收断开的客户端。缺少能力时需更新服务，最终关闭期间拒绝新租约。

维护拒绝活动传输，回收 Rust 桥接程序 并阻止新任务，不改变 `enabled`。Rust 桥接程序 启动和烧录共享按端口命名的互斥锁，Job Object 确保设备复用前烧录工具已退出。写入中断会保留就绪失败记录，重试通过镜像校验和 HELLO 后才清除。

就绪记录为 Windows Public 已知文件夹下 `Wayline/device-state` 中的 `COMx.failed`。任何失败记录都会阻止 `port: "auto"`。设备重新枚举可能改变端口号，成功重试只清除新端口记录。通过 `set` 明确配置已验证的新端口，或核实旧设备后处理其记录；不要清除无关失败。系统不会自动关联新旧 USB 身份。

## 离线发布包

从同一检出版本构建主机与固件，再使用新的输出目录。构建和打包不会启动服务、打开端口或刷写设备。

```sh
cargo run --locked -p wayline-tools -- host-build --release
cargo run --locked -p wayline-tools -- desktop-build --release
cargo run --locked -p wayline-tools -- bridge-build --test
cargo run --locked -p wayline-tools -- firmware-build --target esp32s3 --output firmware/build-rust --sdkconfig-defaults "sdkconfig.defaults;sdkconfig.defaults.cardputer"
cargo run --locked -p wayline-tools -- package-verify path/to/firmware/manifest.json
```

Rust 工具现在负责主机、桌面、桥接和固件构建编排、发布目录组装/校验以及 Windows 服务注册。`package-build` 会创建带哈希的 staging 发布目录，`release-verify` 会拒绝缺失、篡改、重复、不安全或未列出的文件。`service-install` 更新服务失败时恢复原配置和运行状态，`service-uninstall` 会先停服再删除。

```sh
cargo run --locked -p wayline-tools -- package-build --input target/release --output build/release --version 0.1.0
cargo run --locked -p wayline-tools -- release-verify build/release
cargo run --locked -p wayline-tools -- service-install --binary "C:/Program Files/wayline/wayline-service.exe" --resident
cargo run --locked -p wayline-tools -- service-uninstall
```

分发前，在干净 Windows x64 和 Cardputer Adv 实机上验证离线安装、首次刷写、更新、中断恢复与 Apple 设备收发。

## 固件与界面

Rust 负责协议分帧、RX/TX、过滤、计数器、对端跟踪、界面与翻译。`firmware/components/radio/radio_hal.c` 封装 ESP-IDF，`radio_ui.cpp` 封装 M5Unified 显示、键盘、音频、背光和 NVS。共享 C 定义位于 `crates/wayline-proto/include/proto.h`，主机与固件布局必须一致。

固件不使用动态内存分配。ESP32-S3 构建使用 Xtensa 工具链，ESP32-C5 使用 ESP RISC-V 工具链。短状态访问使用自旋锁，无线配置和 USB 输出使用独立互斥锁；阻塞驱动调用必须在状态自旋锁之外。界面采用静态 240×135 RGB565 帧缓冲，`ui_orb.rs` 使用有界定点几何。隐藏页面、传输卡片、熄屏或关闭动效时暂停动画。板载操作见[快速开始](QUICKSTART.zh-CN.md#板载界面)。

### 预览与翻译

使用板端同一 Rust 渲染器导出预览：

```sh
cargo run --manifest-path firmware/Cargo.toml --features cardputer-ui --example ui_preview -- build/cardputer-ui
python scripts/preview/render-ui-preview.py --readme
python scripts/preview/render-airdrop-demo.py --direction iphone-to-pc
python scripts/preview/render-airdrop-demo.py --direction pc-to-iphone
```

`build/cardputer-ui/` 包含三种语言的页面、传输卡片、声音设置、动画和校验记录。Python 工具需要 Pillow，并将 README 动图写入 `docs/assets/`。非 Windows 环境可给收发合成器传入 `--font` 指定 CJK 字体。预览不验证无线和硬件行为。

修改 `firmware/assets/locales.json` 后，使用 Pillow、fontTools 和 `firmware/assets/font-source.txt` 指定的字体，一起重新生成字符串表和字体子集：

```sh
python scripts/assets/generate-ui-font.py path/to/NotoSansCJKsc-Regular.otf
cargo fmt --manifest-path firmware/Cargo.toml
cargo test --manifest-path firmware/Cargo.toml --all-features --locked
```

普通构建使用仓库中已有的生成文件和 SIL OFL 许可证。脚本分类、依赖和命令见 [`scripts/README.md`](../scripts/README.md)。

## 板端传输反馈

桌面和 CLI 调用 `wayline_core::enable_board_feedback()`。`wayline-board` 通过 Rust 桥接程序 的 `wayline-radio-ui` 管道每秒最多发布五次更新，并每秒发送心跳。反馈失败不影响文件收发，也不代表授权接受请求。

发送与接收使用独立状态通道。新任务唤醒屏幕并暂停自动熄屏；成功卡片十秒消失，失败/取消卡片三十秒消失，八秒无心跳将活动反馈标记为丢失。Windows 栅格化 Unicode 设备名称字形。

协议 v2 的 `0x08` 携带 `crates/wayline-proto/src/transfer.rs` 中定义的 1,152 字节帧。`0x09` 查询方向 0/1，通过 `0x89` 返回 32 字节头。管道接受 `0 + frame` 或 `1 + direction`，返回状态 0 和查询数据（拒绝为 1），再等待 `0xff`。发布仅确认 Rust 桥接程序 入队，查询确认固件状态。

Rust 桥接程序 与固件运行时，验证文件内容、状态通道、拒绝与取消：

```sh
cargo run -p wayline-core --example board_loopback
cargo run -p wayline-core --example board_loopback -- --failure-cases
```

## 数据通路与基准测试

### 实验性接收聚合

可选 A-MPDU 接收支持 Apple → ESP32 → PC。固件处理 ADDBA/DELBA 与硬件 Block Ack，Rust 桥接程序 不需要额外参数。普通构建默认关闭，尚未实现 PC → Apple 发送聚合。吞吐与 BA 时序需要实机验证。

```sh
cargo test --locked --manifest-path firmware/Cargo.toml --all-features
```

输出和构建记录位于 `firmware/build-rust-aggregation/`，附加配置为 `sdkconfig.defaults.aggregation`；普通打包使用 `firmware/build-rust/`。释放串口后，在已激活的 ESP-IDF 环境刷写：

```sh
$fw = (Resolve-Path firmware).Path
idf.py -C $fw -B "$fw\build-rust-aggregation" -D "SDKCONFIG=$fw\build-rust-aggregation\sdkconfig" -p COM4 flash
```

将 COM4 换成实际端口，完成后重新打开桌面。对比时固定板型、Apple 设备、信道、文件和摆放位置，检查文件哈希、取消、重复传输、断开重连与丢包恢复。独立抓包须确认 ADDBA、聚合 MPDU 和 BA，回应入队不等于无线协商成功。

查询计数器前释放串口，计数器保留到设备复位：

```sh
cargo test --locked -p wayline-proto -p wayline-radio
```

`ampdu_mpdu` 统计接纳的聚合 MPDU，`ampdu_rx_drop` 统计上行丢帧，`accepted` 统计 ADDBA 回应入队。`response_errors`、`control_drop` 和 `tx_report_drop` 显示队列/驱动失败。`MSG_STATS` 请求体 `BA` 返回 56 字节 v1 响应，字段见 [proto.h](../crates/wayline-proto/include/proto.h)。

策略只允许一个对端/TID、2–16 个 MPDU、立即 BA，且不允许 A-MPDU 内含 A-MSDU。重复请求保留硬件位图，建立失败不留下新会话。DELBA、空闲超时、主机丢失或无线更改会拆除会话；代次检查使过期请求失效，有界控制/上行队列保证 Wi-Fi 回调不阻塞。

C HAL 在未关联 STA 上使用接收 BA 表槽位 0，不兼容 AP/SoftAP。ESP-IDF 内部 BA API 要求 SDK 归档哈希匹配[无线 CMake 配置](../firmware/components/radio/CMakeLists.txt)。调用位于配置互斥锁下、状态自旋锁外；表项回读仅确认编程结果，默认启用前仍需验证无线时序。

### 流式处理与编解码基准

固件使用静态 4096 字节缓冲批量发送已排队的 RX 与 TX 完成消息，每批最多八帧，各自保留协议 v2 CRC/COBS 分隔。USB 写入前归还环形队列项，刷新期间最多持有一个待处理项。命令使用独立编码器，仅在完整写入期间持有 TX 锁；空队列不等待凑批。RX 环形队列为 48 KiB，USB TX/RX 缓冲各为 8 KiB。

双核原生 USB 构建将驱动初始化、对应中断和上行任务放在 Wi-Fi 任务的另一核心，卸载驱动须使用安装核心。UART/单核构建使用常规初始化，共享 CRC/COBS 编解码器以 `opt-level = 3` 编译。

释放串口后运行：

```sh
cargo test --locked -p wayline-proto -p wayline-radio
```

`MSG_STATS` 请求体 `UP` 返回十四个 u32le 字段（56 字节，v1），包含批量统计、驱动字节数、编码/写入/互斥锁计时、错误、最大值、堆与中断核心，见 [proto.h](../crates/wayline-proto/include/proto.h)。总量按 u32 回绕，评估时需同时记录丢帧、传输耗时和文件哈希。

共享编解码器在一次 CRC/COBS 遍历中处理分段输入。Rust 桥接程序 同样按最多 4096 字节/八帧批量写入，保持 FIFO、部分写入与背压处理。接收将 DVZip 流式解码到有界 CPIO 解包器，发送将 CPIO 流式压缩为 gzip/DVZip 临时归档。成功须经过边界校验和接收端最终确认。

```sh
cargo run --release -p wayline-proto --example codec_bench
cargo run --release -p wayline-core --example archive_bench
cargo test --manifest-path vendor/owly-airdrop/Cargo.toml --all-features --lib --test vectors --locked
cargo test -p wayline-core --locked
```

主机基准测量编解码 CPU 成本。无线吞吐须在固定板型、信道和文件的实机环境测量，同时记录耗时、完整性和丢帧计数。
