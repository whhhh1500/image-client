# 日志说明

日期：2026-08-30

## 目录与轮转

- 数据库：`<用户目录>/ImageClient/image-client.db`
- 日志：`<用户目录>/ImageClient/logs/`
- 格式：JSON Lines，一行一个事件。
- 文件：`image-client-YYYY-MM-DD.log`；超过限制后使用 `.1.log`、`.2.log` 分卷。
- 单文件严格小于 10 MiB。
- 每个自然日使用独立文件，最多保留最近 10 个自然日。

状态栏的“日志”按钮可以直接打开目录。

## 覆盖范围

- 应用启动、退出、panic、前端全局异常。
- 用户点击、表单变化、页面可见性和前端性能长任务。
- 所有前端 IPC 请求及耗时。
- 前端日志每 250ms 或累计 50 条批量写入，避免详细日志反过来拖慢界面。
- 前端日志后端不可用时采用 5 秒退避并保留有限队列，避免浏览器调试模式产生重试风暴。
- SQLite 打开、查询类型、语句、行数、耗时和错误；不记录绑定值。
- 配置、项目、Prompt、资产和任务状态变化。
- REST 请求路径、功能路由、状态码与耗时。
- 图像请求、下载、文件大小和总耗时。
- 视频任务创建、每次轮询、下载、分段、拼接和混音耗时。
- LLM 请求模型、总耗时、响应头时间、首 token 时间和 token 用量。
- 所有远程错误响应和流式响应均有大小上限，避免异常服务端响应耗尽内存。

## LLM 指标

- `responseHeaderMs`：收到 HTTP 响应头的耗时。
- `firstTokenMs`：流式响应中收到首个文本或工具调用增量的耗时。
- `durationMs`：整个 LLM 请求完成耗时。
- `inputTokens` / `outputTokens` / `totalTokens`：优先使用供应商返回的 usage。
- `tokenUsageSource=estimate_ascii4_nonascii1`：供应商没返回 usage，ASCII 约按 4 字符/Token、非 ASCII 约按 1 字符/Token 估算。
- `responseMode=non_stream`：网关不支持流式，此时无法准确得到首 token，字段为 `null`。

## 隐私

日志不写入 API Key、Authorization、密码、Token、完整 Prompt、完整模型输出、媒体二进制或 SQL `bindValues`。正文仅记录字符数、字节数、哈希或是否存在。
