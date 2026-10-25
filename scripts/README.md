# 开发脚本

本项目的资源生成与预览脚本统一放在此目录，按用途分类并纳入版本控制。
从仓库根目录执行下列命令；脚本根据自身位置定位源码和输出目录。

| 目录 | 脚本 | 用途 |
| --- | --- | --- |
| `assets/` | `generate-desktop-icons.cjs` | 从品牌矢量生成桌面与托盘图标 |
| `assets/` | `generate-firmware-logo.cjs` | 生成固件启动 Logo 的 RLE 数据和 Rust 常量 |
| `assets/` | `generate-ui-font.py` | 从翻译源和 Noto CJK 字体生成字符串表及字体子集 |
| `preview/` | `render-ui-preview.py` | 将固件渲染帧组合为三语言预览及 README 动画 |
| `preview/` | `render-airdrop-demo.py` | 合成 Apple 与电脑双向收发的示意动画 |
| `lib/` | `load-sharp.cjs` | 供 Node 生成器共用的 `sharp` 加载器 |

## 资源生成

Node 生成器需要 Node.js 和 `sharp`；可通过 `WAYLINE_SHARP_MODULE` 指定已安装的模块路径。
Python 工具需要 Pillow 和 fontTools：

```sh
python -m pip install Pillow fonttools
node scripts/assets/generate-desktop-icons.cjs
node scripts/assets/generate-firmware-logo.cjs
python scripts/assets/generate-ui-font.py path/to/NotoSansCJKsc-Regular.otf
cargo fmt --manifest-path firmware/Cargo.toml
```

字体来源及校验值见 [`font-source.txt`](../firmware/assets/font-source.txt)。修改
[`locales.json`](../firmware/assets/locales.json) 后，应一起生成字符串表与字体子集，并保留 OFL 许可证。
普通构建直接使用仓库中已有的生成资源。

## 界面与收发预览

先通过板端同一 Rust 渲染器导出 `build/cardputer-ui/` 中的帧，再运行 Python 合成器：

```sh
cargo run --manifest-path firmware/Cargo.toml --features cardputer-ui --example ui_preview -- build/cardputer-ui
python scripts/preview/render-ui-preview.py --readme
python scripts/preview/render-airdrop-demo.py --direction iphone-to-pc
python scripts/preview/render-airdrop-demo.py --direction pc-to-iphone
```

预览、联系图和校验记录保存在 `build/cardputer-ui/`；README 动画输出到 `docs/assets/`。
非 Windows 环境可用 `render-airdrop-demo.py --font <字体文件>` 指定 CJK 字体。
手机与桌面面板为示意，传输数据为模拟数据；这些工具不操作串口、服务或设备。

## 构建与测试

首次准备本机环境见 [Windows / Linux 环境配置](../docs/SETUP.zh-CN.md)。`cargo run --locked -p wayline-tools -- firmware-check` 检查已安装的固件工具链，不访问设备。

构建、测试、打包和服务管理使用 Rust 工具 `wayline-tools`，详见
[开发指南](../docs/DEVELOPMENT.zh-CN.md)。新增脚本按用途归入上述目录，共用加载逻辑放入 `lib/`；
下载依赖、临时文件和生成预览分别放入 `.cache/` 或 `build/`，不纳入版本控制。
