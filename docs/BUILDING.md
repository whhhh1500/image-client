# 构建与发布

## 环境

- Node.js 24
- pnpm 10
- Rust stable
- 当前平台的 [Tauri 2 前置依赖](https://v2.tauri.app/start/prerequisites/)

Linux 还需要 WebKitGTK 4.1、AppIndicator、librsvg、patchelf 等系统包；仓库工作流给出了 Ubuntu 22.04 的安装命令。

## 本地开发

```bash
pnpm install --frozen-lockfile
pnpm tauri dev
```

## 检查

```bash
pnpm test
cargo test --locked --manifest-path src-tauri/Cargo.toml
pnpm release:check
```

`release:check` 会确认 `package.json`、`Cargo.toml`、`tauri.conf.json` 的版本一致，并验证 1024×1024 自定义主图标及各平台打包图标齐全。

## 当前平台打包

```bash
pnpm release:build
```

脚本会依次执行前端测试、Rust 测试和当前平台的便携发布打包。最终文件位于忽略的 `release-exe/版本/` 下；脚本只接受 Windows x64、macOS Apple Silicon 或 Linux x64，其他架构当前不作为发布目标。

- Windows x64：`Image-Client_版本_x64_portable.exe`。该文件是 Tauri 的 release 可执行文件，不包含安装程序，也不随附 WebView2；Windows 10/11 通常已提供 WebView2 Runtime。
- macOS Apple Silicon：`Image-Client_版本_aarch64.app.tar.gz`。解压后将 `.app` 移到 Applications 或其他目录。
- Linux x64：`Image-Client_版本_amd64.AppImage`。按发行版要求授予可执行权限后运行。

如需更新应用图标，替换带透明通道的 `src-tauri/icons/app-icon-source.png`，再运行：

```bash
pnpm release:icon
```

## GitHub 发布

1. 同步三处版本号并更新 `CHANGELOG.md`。
2. 提交全部源码变更，确认工作区干净。
3. 创建并推送与版本一致的标签，例如 `v0.1.0`。
4. `Publish release` 工作流会构建 Windows x64 portable.exe、macOS Apple Silicon `.app.tar.gz` 和 Linux x64 AppImage，并上传到同一个 GitHub Release。

发布流水线会让 Linux 全量测试与三个目标平台的打包并行执行，所有任务成功后再一次性创建 Release。主分支和 PR 只运行 Linux 前后端测试；三平台 release profile 预热仅在手动触发 `Cross-platform verification` 时运行。标签构建使用相同 key 只读复用，并在上传和发布前通过文件名 allowlist 验证，确保 Release 恰好包含这三个文件。

发布包当前没有商业代码签名或公证。正式分发前可在仓库 Secrets 中接入各平台签名凭据；不要将证书或密钥写进源码。
