<p align="center">
  <img src="public/app-icon.png" width="112" alt="Image-Client 图标">
</p>

<h1 align="center">Image-Client</h1>

<p align="center">本地优先的 AI 图像、视频与小说漫画生产桌面客户端。</p>

<p align="center">
  <a href="https://github.com/whhhh1500/image-client/releases/latest">下载最新版</a> ·
  <a href="CHANGELOG.md">版本记录</a> ·
  <a href="docs/BUILDING.md">构建说明</a>
</p>

## 功能

- 图像与视频生成，支持参考图、历史记录和本地资产管理。
- 小说转漫画工作区，覆盖分析、章节、分镜、批量生成与单页重画。
- 通用短剧流水线，可按阶段调用 Agent 并管理提示词。
- 多项目生产档案与供应商配置，数据默认保存在本机。
- 应用右下角内置简明使用说明与版本记录。

## 下载

在 [GitHub Releases](https://github.com/whhhh1500/image-client/releases) 下载对应平台安装包：

- Windows：NSIS 安装程序
- macOS：Apple Silicon 与 Intel DMG
- Linux：AppImage 与 DEB

当前发布包未进行商业代码签名，操作系统首次打开时可能显示安全提示。请只从本仓库 Releases 下载并核对发布来源。

## 快速开始

开发环境需要 Node.js 24、pnpm 10、Rust stable，以及对应系统的 [Tauri 2 前置依赖](https://v2.tauri.app/start/prerequisites/)。

```bash
pnpm install --frozen-lockfile
pnpm tauri dev
```

首次运行后，在“设置”中填写所用服务商的 API 地址、模型和密钥。密钥与业务数据保存在本地应用数据目录，不应提交到 Git。

## 验证与打包

```bash
pnpm test
cargo test --locked --manifest-path src-tauri/Cargo.toml
pnpm release:build
```

`pnpm release:build` 会先校验三处版本号和自定义图标，再执行前后端测试并生成当前系统的原生安装包。更完整的平台依赖、产物位置和发版流程见 [docs/BUILDING.md](docs/BUILDING.md)。

## 本地接口

应用可按需启用本地 REST 接口，供本机自动化流程调用。接口仅应绑定回环地址；字段与示例见 [docs/OPEN_API.md](docs/OPEN_API.md)。

## 数据与安全

- 项目、历史和配置默认位于系统应用数据目录。
- `.env`、数据库、运行日志、缓存、测试产物和打包目录均已加入忽略列表。
- 发布前请检查 `git status`，不要提交真实 API 密钥、私有素材或生成数据。

## 许可证

本仓库目前未附带开源许可证。代码可公开查看，但不自动授予复制、修改或再分发权利。
