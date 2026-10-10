# 构建与发布

桌面外壳是 [Wails v2](https://wails.io)：React 前端在 `src/`，Go 后端在 `internal/`，入口为 `main.go`。

## 环境

- Node.js 24
- pnpm 10
- Go 1.27
- Wails v2 CLI：`go install github.com/wailsapp/wails/v2/cmd/wails@v2.16.0`
- Windows：WebView2 Runtime（Windows 10/11 通常已自带）
- macOS：Xcode Command Line Tools

## 本地开发

```bash
pnpm install --frozen-lockfile
pnpm desktop:dev
```

`desktop:dev` 会启动 Vite（端口 1420）并编译运行 Go 后端，修改 Go 代码后自动重启。只调界面时也可以单独运行 `pnpm dev`，但此时没有后端，调用会提示"桌面运行时不可用"。

## 检查

```bash
pnpm test
go test ./internal/...
pnpm release:check
```

`release:check` 会确认 `package.json` 与 `wails.json`（`info.productVersion`）的版本一致，并验证 1024×1024 自定义主图标与 Windows 图标齐全。

## 当前平台打包

```bash
pnpm release:build
```

脚本会依次执行前端测试、Go 测试和当前平台的便携发布打包（`wails build`，通常一分钟内完成）。最终文件位于忽略的 `release-exe/版本/` 下；脚本只接受 Windows x64 或 macOS Apple Silicon，其他平台当前不作为发布目标。

- Windows x64：`Image-Client_版本_x64_portable.exe`。单个可执行文件，不包含安装程序，也不随附 WebView2。
- macOS Apple Silicon：`Image-Client_版本_aarch64.app.tar.gz`。解压后将 `.app` 移到 Applications 或其他目录。

如需更新应用图标，替换带透明通道的 `build/appicon.png`（1024×1024）和 `build/windows/icon.ico`。

## GitHub 发布

1. 同步 `package.json` 与 `wails.json` 的版本号并更新 `CHANGELOG.md`。
2. 提交全部源码变更，确认工作区干净。
3. 创建并推送与版本一致的标签，例如 `v0.1.0`。
4. `Publish release` 工作流会构建 Windows x64 portable.exe 并上传到 GitHub Release；macOS Apple Silicon 需要对该标签手动运行工作流并勾选 `macos`。

发布流水线让 Linux 上的前后端测试与打包并行执行，全部成功后再一次性创建 Release，并在上传前通过文件名 allowlist 验证产物。主分支和 PR 只运行测试；手动触发 `Cross-platform verification` 会额外在 Windows 与 macOS 上完整构建一次。

发布包当前没有商业代码签名或公证（macOS 包为临时签名）。正式分发前可在仓库 Secrets 中接入各平台签名凭据；不要将证书或密钥写进源码。
