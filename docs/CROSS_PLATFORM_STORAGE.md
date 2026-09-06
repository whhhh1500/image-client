# 跨平台目录与数据库

日期：2026-08-30

## 首次启动生成位置

应用使用统一的用户主目录方案，数据库、配置、日志和默认资产互为同级或子目录：

| 平台 | 数据根目录 |
|---|---|
| Windows | `C:\Users\<用户>\ImageClient` |
| macOS | `/Users/<用户>/ImageClient` |
| Ubuntu/Linux | `/home/<用户>/ImageClient` |

目录结构：

```text
ImageClient/
├── image-client.db
├── backend-config.json
├── logs/
└── assets/
```

启动时会自动创建根目录、日志目录和资产目录，再由内置 `rusqlite` 创建数据库并执行版本化迁移。当前 schema 为 v1，使用 `PRAGMA user_version` 记录版本，并拒绝旧程序写入更高版本的数据库。SQLite 使用 bundled SQLite，不依赖系统预装 SQLite。

macOS 和 Linux 上目录权限设置为 `0700`，DB、配置和日志文件设置为 `0600`。Windows 使用当前用户继承的 NTFS ACL。

## 配置可移植性

- 默认输出目录内部保存为 `$DEFAULT_ASSETS`，运行时解析为当前平台的 `~/ImageClient/assets`。
- 相对输出路径解析到当前平台的 `~/ImageClient` 下。
- 如果在 macOS/Linux 读取到 `C:\...`，或 Windows 读取到 `/Users/...`、`/home/...`，会记录警告并回退默认资产目录，避免创建错误路径。
- 用户明确选择的自定义绝对目录只适用于原平台；迁移到另一平台后应重新选择。

SQLite 文件格式本身跨平台，但当前历史资产记录保存的是生成时的绝对文件路径。因此“新平台首次建库”没有问题；直接把旧 DB 从 Windows 复制到 macOS/Linux 并不能自动修复历史资产路径。跨平台迁移历史项目时，应重新导入资产或使用后续专门的导出/导入功能，不能只复制 DB。

## 构建要求

- macOS：在 Mac 上安装 Xcode Command Line Tools，然后运行 `pnpm tauri build`。
- Ubuntu：建议 Ubuntu 22.04，安装 WebKitGTK 4.1 等 Tauri 原生依赖后运行 `pnpm tauri build`。
- Windows 不能直接产出可验证的 macOS `.app/.dmg`。仓库已增加 Windows、macOS、Ubuntu 的 GitHub Actions 构建矩阵。

Ubuntu AppImage 已启用媒体框架打包，以提高本地视频播放兼容性。macOS 最低版本设置为 10.15。
