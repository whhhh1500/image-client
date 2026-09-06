# 不调用付费模型的开放接口契约测试：检查版本、功能域、路由命名、日志配置与旧接口兼容。
param([string]$BaseUrl = "http://127.0.0.1:8123")

$ErrorActionPreference = "Stop"
$expected = @(
  "GET /api/v1/health",
  "GET /api/v1/system/info",
  "GET /api/v1/system/config",
  "PUT /api/v1/system/config",
  "GET /api/v1/system/logs",
  "GET /api/v1/system/history-sync",
  "PUT /api/v1/system/provider",
  "GET /api/v1/catalog/tools",
  "GET /api/v1/catalog/models",
  "GET /api/v1/catalog/models/image",
  "GET /api/v1/catalog/models/video",
  "GET /api/v1/catalog/models/llm",
  "GET /api/v1/catalog/providers",
  "POST /api/v1/text/completions",
  "POST /api/v1/agents/director/runs",
  "POST /api/v1/agents/writer/runs",
  "POST /api/v1/agents/storyboard/runs",
  "POST /api/v1/agents/consistency/runs",
  "POST /api/v1/agents/qc/runs",
  "POST /api/v1/agents/orchestrations",
  "POST /api/v1/media/images/generations",
  "POST /api/v1/media/videos/generations",
  "POST /api/v1/assets/text",
  "POST /api/v1/assets/media"
)

$healthResponse = Invoke-WebRequest "$BaseUrl/api/v1/health" -Headers @{ Origin = "http://localhost:1420" } -TimeoutSec 5
$health = $healthResponse.Content | ConvertFrom-Json
if ($health.status -ne "ok" -or $health.apiVersion -ne "v1") { throw "health 契约不正确" }
if (-not $healthResponse.Headers["x-request-id"]) { throw "响应缺少 x-request-id" }
if ($healthResponse.Headers["access-control-allow-origin"] -ne "http://localhost:1420") { throw "CORS 响应头不正确" }

$info = Invoke-RestMethod "$BaseUrl/api/v1/system/info" -TimeoutSec 5
if ($info.authentication -ne "none") { throw "当前约定应为无鉴权" }
if ($info.historyPersistence -ne "sqlite") { throw "开放接口产物没有声明持久化到 SQLite 历史" }
if ($info.historySynchronization.mode -ne "event_driven" -or $info.historySynchronization.polling) {
  throw "前台历史同步必须为事件驱动且不得定时轮询"
}
if (-not $info.historySynchronization.manualRefresh) { throw "页面缺少手动刷新兜底" }
$sync = Invoke-RestMethod "$BaseUrl/api/v1/system/history-sync" -TimeoutSec 5
if ($sync.mode -ne "event_driven" -or $sync.polling) { throw "历史同步状态接口不正确" }
$actual = @($info.routes | ForEach-Object { "$($_.method) $($_.path)" })
$missing = @($expected | Where-Object { $_ -notin $actual })
$extra = @($actual | Where-Object { $_ -notin $expected })
if ($missing.Count -or $extra.Count) {
  throw "接口清单不匹配。缺少=$($missing -join ', ')；额外=$($extra -join ', ')"
}

foreach ($route in $info.routes) {
  if ($route.path -notmatch '^/api/v1/(system|catalog|text|agents|media|assets)(/[a-z0-9-]+)*$' -and $route.path -ne '/api/v1/health') {
    throw "路由命名不规范: $($route.method) $($route.path)"
  }
  if ($route.name -notmatch '^[a-z][a-z0-9_]*$') { throw "操作名不规范: $($route.name)" }
}

$logs = Invoke-RestMethod "$BaseUrl/api/v1/system/logs" -TimeoutSec 5
if (-not $logs.dailyFiles -or $logs.maxFileBytesExclusive -ne 10485760 -or $logs.retentionDays -ne 10) {
  throw "日志策略接口与要求不一致"
}

$tools = Invoke-RestMethod "$BaseUrl/api/v1/catalog/tools" -TimeoutSec 5
if (@($tools.tools).Count -lt 7) { throw "工具清单不完整" }
$models = Invoke-RestMethod "$BaseUrl/api/v1/catalog/models/video" -TimeoutSec 5
if (@($models.items).Count -lt 1) { throw "视频模型目录为空" }
$config = Invoke-RestMethod "$BaseUrl/api/v1/system/config" -TimeoutSec 5
$legacy = Invoke-RestMethod "$BaseUrl/api/health" -TimeoutSec 5
if ($legacy.status -ne "ok") { throw "旧接口兼容失败" }

try {
  Invoke-RestMethod -Method Post -Uri "$BaseUrl/api/v1/media/images/generations" `
    -ContentType "application/json" -Body '{"prompt":"   "}' -TimeoutSec 5 | Out-Null
  throw "非法图像请求未被拒绝"
} catch {
  if ($_.Exception.Response.StatusCode.value__ -ne 400) { throw }
}

Write-Host "PASS api-contract: routes=$($actual.Count) tools=$(@($tools.tools).Count) history=sqlite/event-driven logs=$($logs.directory)" -ForegroundColor Green
Write-Host "config: image=$($config.imageReady) video=$($config.videoReady) llm=$($config.llmReady)"
