# 开放接口

日期：2026-08-30

基础地址：`http://127.0.0.1:8123/api/v1`

按要求不做鉴权。默认只监听 `127.0.0.1`，不要在不可信网络中修改 `API_HOST`。

- REST 不做鉴权。浏览器 CORS 默认只允许 Tauri 与本地开发来源；可通过逗号分隔的 `API_CORS_ORIGINS` 显式增加可信来源。PowerShell、Python、curl 等非浏览器客户端不受 CORS 影响。
- 每个响应包含 `x-request-id`，可与 JSONL 日志关联。
- 单次请求体上限 64 MiB。
- 文本、Prompt、模型名、标签和工具参数均有长度限制；媒体接口会校验容器格式。
- `PUT /system/config` 会持久化后端配置快照，重启后仍可恢复。
- Agent、图像、视频、文本资产和媒体资产接口成功后，产物会登记进统一 SQLite 历史，并向桌面前台触发 `history://changed` 事件。
- 前台收到事件后立即刷新历史，不做定时轮询；用户也可以使用顶栏或资产库中的“刷新历史”按钮主动重读数据库。
- 多个外部调用可以并发写入，SQLite 事务与 busy timeout 用于避免写入覆盖；生成资源使用唯一 ID。

## 功能域

| 功能域 | 路径 | 功能 |
|---|---|---|
| system | `/system/*` | 服务信息、运行配置、供应商切换、日志策略 |
| catalog | `/catalog/*` | 工具、供应商、图像/视频/LLM 模型目录 |
| text | `/text/completions` | 通用文本补全 |
| agents | `/agents/*/runs` | 导演、编剧、分镜、一致性、质检与编排 |
| media | `/media/*/generations` | 图像和视频生成 |
| assets | `/assets/*` | 保存文本与 base64 媒体资产 |

## 历史同步与可选元数据

所有会产生资产的 REST 接口都会保存历史。现有请求参数保持兼容，并支持传入可选的 `projectId`，把外部产物归入指定项目。

- Agent 接口响应新增 `asset`，同时保留原有 `result`。
- 图像和视频接口继续返回 `assets`，返回的每个资产都会写入历史。
- `/assets/text` 支持可选 `projectId`。
- `/assets/media` 支持可选 `label`、`prompt`、`model`、`projectId`。
- 历史记录包含 `origin: rest_api`、模型、生成参数和原始输入来源快照。
- `GET /api/v1/system/info` 的 `historySynchronization` 会返回当前同步策略：`event_driven`、`polling: false`、`manualRefresh: true`。
- `GET /api/v1/system/history-sync` 返回事件监听器是否就绪、已发出/已确认的修订号以及待确认数量，可用于多调用方诊断。

完整机器可读接口清单：`GET /api/v1/system/info`。

## 兼容性

旧 `/api/health`、`/api/config`、`/api/tools`、`/api/text`、Agent 和媒体生成路径继续可用；新集成应使用 `/api/v1`。

## 完备性结论

当前 Rust/Tauri 后端可复用的核心能力均已暴露：配置状态、供应商列表/切换、图像/视频/LLM 模型目录、工具目录、文本生成、5 个 Agent、Agent 编排、图像、视频、文本资产和媒体资产。

项目和 Prompt 版本仍由桌面工作区管理，没有开放完整 REST CRUD；但所有 REST 生成/保存接口的产物已经统一登记到资产历史，桌面前台会通过后端事件即时发现外部调用产生的新记录。

## 测试

```powershell
& "D:\cc\image-client\scripts\api-contract-test.ps1"
```

该脚本不调用付费模型，会检查 23 个 v1 路由、命名规范、功能域、日志策略和旧接口兼容。
