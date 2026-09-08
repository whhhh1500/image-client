# 开放接口

日期：2026-08-30

基础地址：`http://127.0.0.1:8123/api/v1`

按要求不做鉴权。默认只监听 `127.0.0.1`，不要在不可信网络中修改 `API_HOST`。

- REST 不做鉴权。浏览器 CORS 默认只允许 Tauri 与本地开发来源；可通过逗号分隔的 `API_CORS_ORIGINS` 显式增加可信来源。PowerShell、Python、curl 等非浏览器客户端不受 CORS 影响。
- 每个响应包含 `x-request-id`，可与 JSONL 日志关联。
- 单次请求体上限 64 MiB。
- 文本、Prompt、模型名、标签和工具参数均有长度限制；媒体接口会校验容器格式。
- `PUT /system/config` 会持久化后端配置快照，重启后仍可恢复。
- `GET/PUT /system/llm` 是文本 Agent 专用配置接口。PUT 请求为 `{ "url": "https://.../v1", "key": "...", "model": "..." }`，只更新 LLM；GET 不返回 Key。
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
- `/agents/*/runs` 是独立文本 Agent 调用，不等同于桌面短剧生产闭环。其响应和资产元数据标记 `workflowStatus: unreviewed`；只有经过工作区版本、独立审稿、QC 资产绑定和生产清单门禁的内容才属于已审视频工作流。
- `/media/videos/generations` 是通用直调视频接口，产物标记 `workflowStatus: direct_api_unreviewed`，不会伪装成已通过分镜/QC 的视频资源。
- 图像和视频接口继续返回 `assets`，返回的每个资产都会写入历史。
- `/assets/text` 支持可选 `projectId`。
- `/assets/media` 支持可选 `label`、`prompt`、`model`、`projectId`。
- 历史记录包含 `origin: rest_api`、模型、生成参数和原始输入来源快照。
- `GET /api/v1/system/info` 的 `historySynchronization` 会返回当前同步策略：`event_driven`、`polling: false`、`manualRefresh: true`。
- `GET /api/v1/system/history-sync` 返回事件监听器是否就绪、已发出/已确认的修订号以及待确认数量，可用于多调用方诊断。

完整机器可读接口清单：`GET /api/v1/system/info`。

## 视频生成参数

`POST /api/v1/media/videos/generations` 使用模型能力化参数：

- `mode`：`text`、`first_frame` 或 `reference`。
- `images`、`videos`、`audios`：无凭据的公网 HTTPS URL 数组；本机、内网、链路本地和保留 IP 会被拒绝。桌面内部调用还可单独传入同项目的本地**图片身份**，native 只在请求期间将其解析为临时 data URL；本地视频和音频不属于该路径。
- `durationS`、`resolution`、`aspectRatio`：必须符合所选模型能力。
- 首帧模式由参考图决定画面几何，Provider 请求不会额外提交可能冲突的 `aspect_ratio`。
- Drama Video V2 / Fast 的参考素材总数最多为 12 项；各类型上限仍需同时满足。
- `GET /api/v1/catalog/models/video` 的 `items` 来自当前视频 API Key 的 `/v1/models`。已配置 Key 后，鉴权失败、网络失败或空目录会明确报错，不再静默伪装成本地回退成功；`capabilities` 返回界面和服务端共同使用的模式、时长、清晰度及参考素材上限。
- Provider 返回的 task ID 会进入视频资产 ID（`zzone:<taskId>`）及历史参数 `providerTaskId` / `providerTaskIds`。

视频产物只登记为 `video` 资产并保存在“视频”输出目录；图像产物仍使用独立的 `image` 资产和“图片”目录。

桌面视频工作区按镜头编排：每镜有独立 Prompt 和模型支持的时长，每镜单独调用一次生成接口并登记为独立视频资产。生成结束不会自动拼接；用户可在视频历史中选择多个镜头，显式点击“拼接所选镜头”后由本地媒体代码生成新的拼接资产。声音混合与字幕暂不处理。

## 视频分镜 Markdown

短剧视频分镜只接受固定标题结构的 Markdown：文档以 `# 视频分镜` 开头，每镜使用 `## 第N镜`，并包含场次、景别、构图、光线、运镜、画面动作、情绪、时长、起始状态、动作过程、结束状态、承接镜头、画风锚、场景锚、角色锚、道具锚、参考方式、参考资产、来源对白和视频 Prompt。对白只作来源，不进入当前视频生成 Prompt。

分镜工作区确认后可将每镜 Prompt 和时长发送到视频工作区。一个 Markdown 镜头对应一次 API 请求和一个视频资源；不会隐式拆镜。视频历史保存来源分镜资产 ID 和锚点 ID，建立完整溯源。

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
