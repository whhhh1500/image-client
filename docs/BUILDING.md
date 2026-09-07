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

脚本会依次执行前端测试、Rust 测试和 Tauri 原生打包。产物位于 `src-tauri/target/release/bundle/` 下的平台子目录。

如需更新应用图标，替换带透明通道的 `src-tauri/icons/app-icon-source.png`，再运行：

```bash
pnpm release:icon
```

## GitHub 发布

1. 同步三处版本号并更新 `CHANGELOG.md`。
2. 提交全部源码变更，确认工作区干净。
3. 创建并推送与版本一致的标签，例如 `v0.1.0`。
4. `Publish release` 工作流会构建 Windows NSIS、macOS DMG（Apple Silicon/Intel）、Linux AppImage/DEB，并上传到同一个 GitHub Release。

发布流水线会让测试与四个平台的打包并行执行，所有任务成功后再一次性创建 Release。主分支跨平台验证为 Windows、Linux、Apple Silicon 和 Intel Mac 分别保存一份稳定的 Rust target 缓存；标签构建使用相同 key 只读复用，不再因 job 名不同而完全失配，也不会像细粒度编译缓存那样产生上千个小缓存并触发 GitHub 上传限流。

发布包当前没有商业代码签名或公证。正式分发前可在仓库 Secrets 中接入各平台签名凭据；不要将证书或密钥写进源码。
