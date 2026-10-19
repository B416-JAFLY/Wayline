# omdrop-windows

通过 USB 连接的 ESP32 在 Windows 与 Apple 设备之间收发 AirDrop 文件。桌面应用、AirDrop 核心、服务管理和固件应用使用 Rust，AWDL 由 OWL 提供，硬件驱动使用 ESP-IDF 和 M5Unified。

固件支持 ESP32-S3，使用 M5Stack Cardputer Adv、原生 USB Serial/JTAG 和 2.4 GHz 信道 6。电脑需要蓝牙 LE 广播能力，Wi-Fi 收发由 ESP32 承担。ESP32-C5 和 5 GHz 尚不支持。默认固件不启用帧聚合；[实验性接收聚合构建](docs/DEVELOPMENT.md#experimental-receive-aggregation)提供 ADDBA 协商与硬件 Block Ack。实机测试中，USB 上行优化降低了聚合接收的队列丢包并改善速度；实际收益仍需按设备验证。

[English](README.md) · [快速开始](docs/QUICKSTART.zh-CN.md) · [开发指南](docs/DEVELOPMENT.md)

## ESP32 界面演示

![Cardputer Adv 完整界面演示](docs/assets/cardputer-demo.gif)

53 秒循环展示开机 Logo、连接点阵球、分享指南、偏好与声音设置、帮助与设备信息、三语言切换，以及发送、双向传输、完成和中断状态。演示使用固件同一渲染器，传输状态由示例数据驱动。

## AirDrop → ESP32 → Omdrop

![AirDrop 经 ESP32 桥接到 Omdrop 的交互演示](docs/assets/airdrop-esp32-omdrop.gif)

24 秒模拟 iPhone「照片」选图、点击分享、在系统分享面板选择「隔空投送」与 Omdrop PC，再展示桌面端点击接受、传输、最终确认与保存到「下载 / Omdrop」。ESP32 负责无线桥接，接收授权与文件保存由桌面端完成。手机和桌面面板是界面示意，板端使用实际固件渲染器，传输数据为模拟数据。

## Omdrop → ESP32 → AirDrop

![Omdrop 经 ESP32 向 iPhone 发送照片的交互演示](docs/assets/omdrop-esp32-airdrop.gif)

24 秒模拟 PC 端选择照片、在附近设备中选中 iPhone 并点击发送，随后在 iPhone 的隔空投送请求中点击「接受」。照片经 USB → ESP32 → AWDL 传输；Omdrop 等待接收确认后显示「已送达」，iPhone 展示收到的照片。手机和桌面面板是界面示意，板端使用实际固件渲染器，传输数据为模拟数据。

## 目录结构

| 目录 | 内容 |
| --- | --- |
| `Cargo.toml`、`Cargo.lock` | Windows workspace 和依赖锁定 |
| [`assets/branding`](assets/branding/README.md) | 桌面与固件共用的 Logo SVG 母版、PNG 预览和设计说明 |
| `apps/desktop` | GPUI 桌面、托盘、资源管理器集成和收发界面 |
| `apps/cli` | AirDrop 命令行工具 |
| `crates/omdrop-service` | 链接到桌面的服务模块及独立服务入口 |
| `crates/omdrop-host` | 共用设备名称、用户身份、接收广播标识、保存目录和 AWDL 网卡选择 |
| `crates/omdrop-flash` | 固件包校验、设备维护、烧录、取消和启动检查 |
| `crates/omdrop-core` | 设备发现、蓝牙唤醒、TLS 和文件收发 |
| `crates/omdrop-airdrop`、`crates/omdrop-net` | 归档、plist、DNS 和 HTTP |
| `crates/omdrop-board`、`crates/omdrop-proto` | 板端状态反馈与主机/固件共享协议 |
| `crates/omdrop-radio`、`crates/omdrop-transport` | 串口诊断与通信适配 |
| `firmware` | ESP32 Rust 应用、界面、声音、字体和翻译 |
| `firmware/components/radio` | ESP-IDF/M5Unified 硬件桥接 |
| `native/owl` | Windows OWL、Wintun 适配和测试 |
| `native/owl/patches`、`native/owl/pins` | OWL 补丁和固定依赖版本 |
| `vendor/stacksafe-macro` | GPUI 间接宏依赖的 Rust 兼容性补丁 |
| `scripts`、`tests`、`justfile` | 构建、安装、诊断和测试入口 |

`firmware` 是独立 Cargo 项目，拥有自己的锁文件和 Xtensa 编译配置。共享 C 协议头位于 `crates/omdrop-proto/include/proto.h`。构建产物、下载的依赖和 ESP-IDF 生成文件不纳入版本控制。

## 安装与使用成品包

完整 Windows x64 离线包包含桌面程序、OWL、Wintun、Cardputer Adv 固件、固定版本烧录工具和所需运行库。解压后双击 `install.cmd`；安装器校验全部文件后提升权限注册服务，再从原来的普通用户进程启动桌面。安装与烧录不调用开发工具，也不临时下载依赖。CLI 另有可选工具包，可使用已安装的桌面或独立服务。详细步骤见[快速开始](docs/QUICKSTART.zh-CN.md)。

更新前从托盘退出桌面并结束 CLI 任务。安装器保留旧程序备份；用户配置、身份和接收文件位于安装目录之外。开发环境中的构建、模拟测试与打包检查不能替代干净 Windows 安装和 Cardputer Adv 实机验收，具体待验收项见[实施方案](docs/CLI_DESKTOP_PLAN.zh-CN.md)。

## 从源码构建与安装 Windows 应用

需要 Git、Rust MSVC 工具链、Visual Studio C++ Build Tools、Windows SDK（含 `fxc.exe`）、MinGW GCC、CMake 和 Ninja。Git 与 GCC 需在 PATH 中；脚本会从 PATH、CMake/ESP-IDF 常用安装目录和 Visual Studio CMake 组件中查找 CMake、Ninja，不修改系统 PATH。从仓库根目录执行：

```powershell
scripts/build-owl.ps1
scripts/build-release.ps1
```

OWL 脚本按 `native/owl/pins` 获取源码、应用补丁，并下载和校验 Wintun。`build-release.ps1` 构建桌面、CLI、独立服务并记录版本和文件校验值，供发布打包使用。只开发桌面时可运行 `scripts/build-desktop.ps1`。输出位于 `native/owl/build` 和 `target/release`。

自定义工具位置可通过 `-CMakePath <cmake.exe路径>` 和 `-NinjaPath <ninja.exe路径>` 指定。
运行 `scripts/build-owl.ps1 -Test` 会在构建后执行无需硬件的 OWL 回归测试；`just test-owl` 调用同一入口。

从托盘退出正在运行的桌面程序，然后在管理员终端安装：

```powershell
scripts/install-service.ps1
```

此开发者安装入口使用已编译文件；只有显式传入 `-Build` 才会编译。它安装桌面、OWL 和 Wintun，固件与烧录工具通过完整发布包提供。安装目录为 `%ProgramFiles%\omdrop-owl`。以普通权限启动桌面程序，确保资源管理器拖放可用。

桌面与后台使用同一个 `omdrop-desktop.exe`：普通进程运行界面，Windows 通过 `--service --on-demand` 启动高权限后台进程。服务按需启动，`owl.exe` 由后台自动管理。托盘“退出 Omdrop”取消本客户端的传输并停止接收；其他 CLI 会话仍在使用时后台继续运行，最后一个会话退出后才回收 OWL 并退出按需服务。关闭分享卡片只隐藏界面。

无桌面的命令行部署可用 `scripts/install-service.ps1 -Runtime Rust` 安装自动启动的独立服务。`-Uninstall` 删除服务注册，保留安装目录中的文件。

## 桌面使用

电脑保持蓝牙开启；iPhone 开启 Wi-Fi 和蓝牙，将 AirDrop 设为“所有人（10 分钟）”。电脑内置 Wi-Fi 无需开启，也无需连接同一个路由器。

- 拖动文件时，右下角出现分享入口；也可通过托盘、`Ctrl+O` 或文件右键“通过 Omdrop 发送”选择文件。
- 首页球形点阵在无线桥接就绪后缓慢旋转并呼吸缩放；断开后逐渐变暗静止，隐藏卡片后暂停动效。
- 桌面使用双折页图标；托盘在无线桥接就绪时显示蓝色，启动中、未连接或连接出错时显示灰色。隐藏分享卡片后，托盘仍会更新连接状态。
- 窗口圆角保留平滑的透明边缘，页眉图标按屏幕缩放从矢量图绘制。
- 选择接收设备后发送。上传进度达到 100% 后，仍会等待接收端确认。
- 收到传输请求时选择接受或拒绝。接收界面显示等待开始、接收进度与完成确认；已知总量时显示百分比进度条，未知总量时显示已接收字节和活动条。完成状态以协议校验和最终回应为准。
- 接收结果以简短提示显示，8 秒后自动收起，也可点击提示右侧 × 关闭。新接收请求或新文件选择会清除旧提示；“选择文件”按钮保持独立可见，完整错误诊断写入接收日志。
- 鼠标悬停提示条可展开完整内容，长文本支持滚动；点击“复制”可复制全文。停留在提示或详情上时暂停自动收起，移开后重新计时。
- 接收文件默认保存到下载目录的 `Omdrop` 子目录，可在设置 → 接收 AirDrop → 更改保存目录中选择其他文件夹。设置与 CLI 共用，CLI 的 `--save` 可覆盖本次接收目录。更改目录不会移动已有文件，接收活动期间不可更改。
- 分享卡片齿轮按钮可修改设备名称，支持中文，最多 64 个字符；CLI 共用此名称与身份。
- 分享页面顶部的“设备固件”提供明确的串口和板型选择、首次安装、保留设置更新、进度、取消和重试。保留设置更新要求旧固件可识别且分区布局一致；设备未提供应用版本时显示未知。
- 右上角 × 或 Esc 隐藏卡片；托盘“退出 Omdrop”或 `Ctrl+Q` 释放本客户端会话。烧录期间退出需要确认，取消后会等待烧录进程退出再释放设备。

烧录失败后若设备重新枚举为另一个 COM 端口，新端口重刷成功也不会自动清除旧端口的失败记录；自动无线发现会继续暂停。需显式配置已验证的新端口，或核实旧设备后处理旧记录，见[维护恢复限制](docs/DEVELOPMENT.md#service-configuration-and-ipc)。该场景仍待实机验收。

首次启动会注册当前用户的文件右键菜单和“发送到 → Omdrop AirDrop”。Windows 11 的文件右键入口通常位于“显示更多选项”。退出后运行 `omdrop-desktop.exe --unregister-shell` 可移除这两个入口。

CLI 自动选择 AWDL 网卡，并按需取得服务会话。交互接收逐次询问；脚本或 JSON 模式接收必须显式加 `--accept-all`，发送必须指定无歧义的 `--to`。`status` 和 `doctor` 只读取状态，不启动服务或占用串口：

```powershell
omdrop-cli status --json
omdrop-cli discover
omdrop-cli send --to "iPhone" -- "C:\照片\photo.jpg"
omdrop-cli receive
omdrop-cli doctor --json
```

JSON Lines 格式、退出码和本地 TCP 诊断见[开发指南](docs/DEVELOPMENT.md#cli-and-radio-diagnostics)。

## 构建与刷写 Cardputer Adv

安装 ESP-IDF 6.0 和 Rust，从仓库根目录执行：

```powershell
cargo install espup --locked
espup install --targets esp32s3 --std
scripts/build-firmware.ps1
```

脚本支持已激活的 ESP-IDF 环境、EIM 安装目录或 `-IdfProfile <激活脚本>`。输出为 `firmware/build-rust/owl-radio.bin`，成功后记录本次构建的镜像、偏移和校验值。发布固件包及离线安装包的命令见[开发指南](docs/DEVELOPMENT.md#offline-release-packages)。成品用户通过桌面的“设备固件”页面安装，无需下面的开发者工具。

刷写前从托盘退出桌面程序，关闭串口监视器；独立服务需先停止。在已激活 ESP-IDF 的终端执行以下命令，将 COM4 换成设备端口：

```powershell
$fw = (Resolve-Path firmware).Path
idf.py -C $fw -B "$fw\build-rust" -D "SDKCONFIG=$fw\build-rust\sdkconfig" -p COM4 flash
```

该命令同时写入 bootloader、分区表和应用。刷写成功后可运行串口回归测试：

```powershell
python tests/firmware_regression.py -p COM4
```

如果设备停在下载模式，松开 G0 后点按 RST。完成后重新打开桌面程序即可启动无线服务。

## 板载界面

界面提供状态、分享指南、偏好设置和更多功能四个页面，支持简体中文、繁體中文和 English。首页显示发送、接收进度及结果；双向传输时同时显示两张卡片。“已连接”表示板子近期收到电脑命令，文件结果以传输状态卡为准。

开机欢迎页使用双折页图标与 Omdrop 字标：两片折页靠拢、文字淡入，约 2.4 秒后进入状态页；任意键可跳过，W 可重播。关闭动效时直接显示完整 Logo。空闲状态页使用立体球形点阵，电脑连接后缓慢旋转并呼吸缩放，断开后渐暗静止；关闭动效、切换页面、显示传输卡片或熄屏时暂停球体动画。

| 按键 | 操作 |
| --- | --- |
| Tab 或 1 / 2 / 3 / 4 | 切换主页面 |
| L | 切换语言 |
| B / M | 切换亮度 / 开关动效 |
| A | 打开声音页；在声音页开关任务提醒 |
| V | 切换音色并试听：轻柔、8-bit、清脆、极简 |
| − / +（或 =） | 在声音页调整音量，每次 5% |
| P | 在声音页试听 |
| T | 设置自动熄屏：关闭、30 秒、2 分钟、5 分钟 |
| S | 立即熄屏，任意键唤醒 |
| H / I | 连接帮助 / 设备信息 |
| W | 重播欢迎动画 |
| R（设置或关于页） | 恢复界面默认设置；Enter 确认，其他键取消 |
| Backspace | 返回上一级或状态页 |

偏好在调整后约 1.2 秒保存。任务声音默认开启、音量 50%、音色“轻柔”；自动熄屏默认关闭。屏幕休眠期间无线通信和声音提醒继续运行。新传输会唤醒屏幕，活动传输期间暂停自动熄屏。

## 开发与测试

安装 `just` 后，从仓库根目录执行：

```powershell
just rust-test
just rust-check
just rust-firmware-test
just test-owl
```

主机协议、服务和固件逻辑测试无需连接设备；AirDrop 互通与无线吞吐需要实机测试。CLI 用法、服务配置、固件字体和界面预览见[开发指南](docs/DEVELOPMENT.md)。

## 鸣谢

感谢以下项目的作者、维护者与贡献者，为 Omdrop 提供了协议研究、实现参考、硬件支持和开发工具：

- **[omdrop-owl-windows](https://github.com/dantdmdarealguy/omdrop-owl-windows)**（dantdmdarealguy）：本项目的直接上游，提供 Windows 端 OWL 移植、ESP32 无线桥接、Wintun 集成及早期 AirDrop 收发实现。
- **[omdrop-owl](https://github.com/t4t5/omdrop-owl)**（t4t5）：提供 Linux 无线后端、OWL 补丁与固定信道无线方案，是 Windows 上游方案的基础。
- **[omdrop](https://github.com/netmojo-systems/omdrop-plugin)** 与 **[omdrop-awdl](https://github.com/netmojo-systems/omdrop-awdl)**（Brent Kearney 及贡献者）：提供 AirDrop 收发、mDNS/BLE 工具及无线后端接口的参考。
- **[OWL / Open Wireless Link](https://github.com/seemoo-lab/owl)**（Milan Stute、SEEMOO / TU Darmstadt 团队）及 **[jedbillyb/owl](https://github.com/jedbillyb/owl)**：提供用户态 AWDL 实现与本项目使用的上游分支。
- **[OpenDrop](https://github.com/seemoo-lab/opendrop)**（SEEMOO）：提供开放的 AirDrop 实现，为 Rust 收发核心的协议实现提供参考。
- **[Windfall](https://github.com/UvejsGj/Windfall)**（UvejsGj，MIT）：提供 DVZip 格式逆向研究，供 `omdrop-airdrop` 的归档解码实现参考。
- **[GPUI](https://gpui.rs/)**（Zed 团队）：提供桌面应用使用的 Rust GPU 加速界面框架。
- **[Wintun](https://www.wintun.net/)**（WireGuard 项目）：提供 Windows 虚拟网络接口驱动，承载 AWDL 网络流量。
- **[ESP-IDF](https://github.com/espressif/esp-idf)**、**[esptool](https://github.com/espressif/esptool)**（Espressif）与 **[espup](https://github.com/esp-rs/espup)**（esp-rs 社区）：提供 ESP32 固件驱动、烧录工具与 Rust 工具链安装支持。
- **[M5Unified](https://github.com/m5stack/M5Unified)** 与 **[M5GFX](https://github.com/m5stack/M5GFX)**（M5Stack）：提供 Cardputer 硬件访问与显示驱动支持。
- **[Noto CJK](https://github.com/notofonts/noto-cjk)**：提供板载中英文字体的源字体；字体来源、子集生成信息及许可证见 [font-source.txt](firmware/assets/font-source.txt) 和 [OFL.txt](firmware/assets/OFL.txt)。

也感谢 Rust 及相关开源生态的维护者，以及参与问题反馈、设备测试和改进的贡献者。

## 许可证

许可证见 [LICENSE](LICENSE)。
