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

- 图像与视频生成，支持参考图、历史记录和本地资产管理；视频逐镜可在实际生成时发送当前项目的本地图片，或使用用户主动托管的公网 HTTPS 图片 URL。
- 小说转漫画工作区，覆盖共享章节文本资产及其修订、作品级多图画风参考、视觉宪法、剧本、分镜、批量生成与单页重画。
- 短剧 Agent 工作流，覆盖导演、编剧、一致性、视频分镜与质检。
- 短剧工作区可上传或选择当前项目的视频作品作为工作区级参考；本地视频用于 Agent 资源元数据与溯源，公网 HTTPS 视频可在模型支持时作为 zzone 生成参考。
- 小说漫画与短剧视频的 AI 优化会读取同一章节/工作区全部已保存产物，并按依赖顺序直接把当前及已有下游文字产物保存为新版本；图片和视频仍由用户主动重新生成。
- 多项目生产档案与供应商配置，数据默认保存在本机。
- 应用右下角内置简明使用说明与版本记录。

## 下载

在 [GitHub Releases](https://github.com/whhhh1500/image-client/releases) 下载对应平台的便携发布文件：

- Windows x64：`Image-Client_版本_x64_portable.exe`，无需安装。
- macOS Apple Silicon：`Image-Client_版本_aarch64.app.tar.gz`，解压后打开 `.app`。
- Linux x64：`Image-Client_版本_amd64.AppImage`。

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

`pnpm release:build` 会先校验三处版本号和自定义图标，再执行前后端测试并生成当前系统的一个便携发布文件。更完整的平台依赖、产物位置和发版流程见 [docs/BUILDING.md](docs/BUILDING.md)。

## 本地接口

应用可按需启用本地 REST 接口，供本机自动化流程调用。接口仅应绑定回环地址；字段与示例见 [docs/OPEN_API.md](docs/OPEN_API.md)。

短剧视频与小说漫画的分镜边界见 [docs/VIDEO_WORKFLOW.md](docs/VIDEO_WORKFLOW.md)。

### AI 优化的作用范围

- 小说漫画：作品设定 → 本章剧本 → 分页分镜 → 有效页 Prompt。优化任一环节时，会从该环节开始直接更新已有下游文字版本；优化某页 Prompt 时默认更新当前页及后续页，可选择包含本章前面的页。
- 短剧视频：改编规划 → 视频剧本 → 视频锚点 → 视频分镜 → QC。系统先完成全部联动优化和独立质量审查，全部通过后才按顺序保存新版本。
- AI 优化会读取同工作区其他文字产物及媒体关联元数据，但不会自动重画漫画或重新生成视频，避免未经确认产生媒体费用。
- 若联动范围内存在未保存草稿、版本冲突或质量审查未通过，操作会停止并保留已有内容。

### 小说漫画画风参考

- 每部小说可上传或选择最多 8 张当前项目的图片作品，作为跨章节共享的画风参考；视频和其他小说的资源不会混入。
- 可调用支持图片输入的文本模型提取可编辑的 Markdown 视觉宪法。参考图只提供线条、色彩、材质、构图等视觉语言，不作为剧情、人物身份、文字、Logo 或具体版式来源。
- 保存后的视觉宪法会进入作品设定、剧本、分镜、页 Prompt 和 AI 优化；生图时同时传递视觉宪法文本与 Provider `references[]`。
- 更新参考图或视觉宪法不会自动重画旧图；旧结果保留并标记需要更新，由用户主动选择重画。

## 数据与安全

- 项目、历史和配置默认位于系统应用数据目录。
- `.env`、数据库、运行日志、缓存、测试产物和打包目录均已加入忽略列表。
- 发布前请检查 `git status`，不要提交真实 API 密钥、私有素材或生成数据。

## 许可证

本仓库目前未附带开源许可证。代码可公开查看，但不自动授予复制、修改或再分发权利。
