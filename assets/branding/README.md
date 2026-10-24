# Wayline 品牌资源

桌面端与 ESP32 固件共用的 Logo 矢量母版、参考预览及设计说明。

| 文件 | 用途 |
| --- | --- |
| [wayline-logo.svg](wayline-logo.svg) | 完整图形与轮廓化字标，透明背景；固件开机 Logo 的来源 |
| [wayline-mark.svg](wayline-mark.svg) | 独立图形，透明背景；桌面程序、页眉及托盘图标的来源 |
| [wayline-logo.png](wayline-logo.png) | 完整 Logo 白底预览，2048 × 768 |
| [wayline-mark.png](wayline-mark.png) | 独立图形透明预览，1024 × 712 |

## 几何与颜色

两片图形共用唯一的 `paper-shape` 路径，深蓝色实例使用
`transform="rotate(180 544 354)"`，两片的边长、角度、曲线和圆角严格对应。
两条相对直边平行，垂直间距约 38.42172 SVG 单位。

浅蓝色为 `#65C7FF`，深蓝色为 `#0A84FF`，字标为 `#242426`。
母版三角形的延长边交点为 `(406.39, 276.76)`、`(767.18, 203.96)`、
`(688.29, 392.45)`，对应相切圆角半径为 13、19、23。

字标使用独立的矢量轮廓，SVG 不依赖位图、系统字体或外部资源。修改图形时同步
两个 SVG 中的 `defs/paper-shape`，让各自的两个 `use` 保持共用同一路径。

## 生成应用资源

[`generate-desktop-icons.cjs`](../../scripts/assets/generate-desktop-icons.cjs) 生成
`apps/desktop/assets` 中的方形 SVG、PNG 和 ICO；托盘已连接时为蓝色，未连接时为灰色。
[`generate-firmware-logo.cjs`](../../scripts/assets/generate-firmware-logo.cjs) 生成
`firmware/assets/boot-logo.rle` 和 `firmware/src/ui_brand.rs`。

生成命令与依赖见[开发指南](../../docs/DEVELOPMENT.zh-CN.md#桌面应用)。
