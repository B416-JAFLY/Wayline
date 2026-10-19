# omdrop-windows

通过 USB 连接的 ESP32，在 Windows 与 Apple 设备之间收发 AirDrop 文件。桌面应用、服务、AirDrop 核心和固件使用 Rust，AWDL 由 OWL 提供。

支持 Windows 10/11 x64 和 M5Stack Cardputer Adv（ESP32-S3、原生 USB Serial/JTAG、2.4 GHz 信道 6）。电脑需要蓝牙 LE 广播能力，Wi-Fi 收发由 ESP32 承担；不支持 ESP32-C5 和 5 GHz。

[English](README.md) · [快速开始](docs/QUICKSTART.zh-CN.md) · [开发指南](docs/DEVELOPMENT.zh-CN.md)

## 界面与收发演示

演示使用实际固件渲染器和模拟传输数据，手机与桌面面板为界面示意。

### ESP32 界面

![Cardputer Adv 界面演示](docs/assets/cardputer-demo.gif)

状态、分享指南、偏好与声音设置，以及三语言切换。

### Apple → Windows

![AirDrop 经 ESP32 桥接到 Omdrop](docs/assets/airdrop-esp32-omdrop.gif)

### Windows → Apple

![Omdrop 经 ESP32 发送到 iPhone](docs/assets/omdrop-esp32-airdrop.gif)

## 安装与使用

1. 解压完整 Windows x64 离线包，双击 `install.cmd`，按提示允许安装后台服务。
2. 用数据线连接 Cardputer Adv，在分享卡片顶部的“设备固件”中选择串口和板型，完成安装或更新。
3. 电脑打开蓝牙；iPhone 打开 Wi-Fi 和蓝牙，将 AirDrop 设为“所有人（10 分钟）”。
4. 拖入文件并选择接收设备；接收时在 Omdrop 中确认接受。

安装包包含固件、烧录工具和运行库，无需安装开发工具或临时下载依赖。桌面请以普通权限运行，确保资源管理器拖放可用。更新前先完成传输，从托盘退出 Omdrop，并结束 CLI 命令。

- 接收文件默认保存到下载目录的 `Omdrop` 文件夹，可在设置中更改；桌面与 CLI 共用名称、身份和保存目录。
- 上传达到 100% 后仍需等待接收端确认，“已送达”才代表发送成功。
- × 或 Esc 隐藏卡片，接收继续运行；托盘“退出 Omdrop”或 `Ctrl+Q` 结束本客户端任务。其他 CLI 会话仍在使用时，后台继续运行。

首次烧录、保留设置更新、板载按键和常见问题见[快速开始](docs/QUICKSTART.zh-CN.md)。CLI 参数、JSON 输出与退出码见[开发指南](docs/DEVELOPMENT.zh-CN.md#cli-与串口诊断)。

## 从源码构建

准备[开发环境](docs/DEVELOPMENT.zh-CN.md#构建与测试)后，从仓库根目录的 PowerShell 执行：

```powershell
scripts/build-owl.ps1
scripts/build-release.ps1
```

输出位于 `native/owl/build` 和 `target/release`。开发者安装使用管理员终端运行 `scripts/install-service.ps1`；固件构建、测试和离线打包见[开发指南](docs/DEVELOPMENT.zh-CN.md)。

## 目录结构

| 路径 | 内容 |
| --- | --- |
| `apps/desktop`、`apps/cli` | GPUI 桌面与命令行入口 |
| `crates/omdrop-service` | 服务、OWL 管理、会话与维护 |
| `crates/omdrop-core`、`crates/omdrop-host`、`crates/omdrop-flash` | 文件收发、共用设置与固件操作 |
| `crates/omdrop-airdrop`、`crates/omdrop-net` | 归档与网络编解码 |
| `crates/omdrop-proto`、`crates/omdrop-board`、`crates/omdrop-radio`、`crates/omdrop-transport` | 共享协议、板端反馈与通信适配 |
| `firmware` | 独立 Rust/ESP-IDF 固件项目，硬件接口位于 `firmware/components/radio` |
| `native/owl` | Windows OWL、固定版本与补丁 |
| `scripts`、`tests`、`assets/branding` | 构建、诊断与[品牌资源](assets/branding/README.md) |

## 鸣谢

感谢以下项目的作者、维护者与贡献者：

- [omdrop-owl-windows](https://github.com/dantdmdarealguy/omdrop-owl-windows)（dantdmdarealguy）：本项目的直接上游，Windows OWL 与 ESP32 无线桥接。
- [omdrop-owl](https://github.com/t4t5/omdrop-owl)（t4t5）：Linux 无线后端与固定信道方案。
- [omdrop](https://github.com/netmojo-systems/omdrop-plugin)、[omdrop-awdl](https://github.com/netmojo-systems/omdrop-awdl)（Brent Kearney 及贡献者）与 [OpenDrop](https://github.com/seemoo-lab/opendrop)（SEEMOO）：AirDrop 协议与收发实现参考。
- [OWL / Open Wireless Link](https://github.com/seemoo-lab/owl)（Milan Stute、SEEMOO / TU Darmstadt）与 [jedbillyb/owl](https://github.com/jedbillyb/owl)：用户态 AWDL 实现。
- [Windfall](https://github.com/UvejsGj/Windfall)（UvejsGj）：DVZip 格式研究。
- [GPUI](https://gpui.rs/)（Zed）与 [Wintun](https://www.wintun.net/)（WireGuard）：桌面界面与虚拟网络接口。
- [ESP-IDF](https://github.com/espressif/esp-idf)、[esptool](https://github.com/espressif/esptool)（Espressif）、[espup](https://github.com/esp-rs/espup)（esp-rs）、[M5Unified](https://github.com/m5stack/M5Unified) 与 [M5GFX](https://github.com/m5stack/M5GFX)（M5Stack）：固件工具链与硬件支持。
- [Noto CJK](https://github.com/notofonts/noto-cjk)：板载字体，来源与许可证见 [font-source.txt](firmware/assets/font-source.txt) 和 [OFL.txt](firmware/assets/OFL.txt)。

## 友情链接

- [LINUX DO 社区](https://linux.do/)

## 许可证

许可证见 [LICENSE](LICENSE)；发布包包含所用依赖的许可证与声明。
