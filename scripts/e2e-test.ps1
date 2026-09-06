# 全链路冒烟测试：REST API 驱动 导演→剧本→分镜→一致性→质检→图像→视频。
# 模型默认用 .env 注释里最便宜的组合，可用参数覆盖。
param(
  [string]$BaseUrl = "http://127.0.0.1:8123",
  [string]$LlmModel = "gemini-3.7-flash",
  [string]$ImageModel = "gemini-2.5-flash-image",
  [string]$VideoModel = "grok-imagine-video",
  [string]$InputFile = "",
  [switch]$SkipText
)

$ErrorActionPreference = "Stop"
if ($InputFile) {
  $story = Get-Content -Raw -Encoding UTF8 $InputFile
} else {
  $story = "深夜便利店。打烊前十分钟，店员苏晚收到一条备注为""救我""的外卖订单，下单地址就是这家店。她必须在三分钟内决定：报警，还是先开门。"
}
$steps = [System.Collections.Generic.List[object]]::new()

function Add-Result([string]$name, [bool]$ok, [string]$detail) {
  $steps.Add([pscustomobject]@{ Step = $name; Ok = $ok; Detail = $detail }) | Out-Null
  if (-not $ok) { Write-Host "[FAIL] $name :: $detail" -ForegroundColor Red }
  else { Write-Host "[ OK ] $name :: $detail" -ForegroundColor Green }
}

function Post-Json([string]$path, $body, [int]$timeoutSec = 600) {
  $json = [System.Text.Encoding]::UTF8.GetBytes(($body | ConvertTo-Json -Depth 8))
  Invoke-RestMethod -Method Post -Uri "$BaseUrl$path" `
    -ContentType "application/json; charset=utf-8" -Body $json -TimeoutSec $timeoutSec
}

function Put-Json([string]$path, $body, [int]$timeoutSec = 60) {
  $json = [System.Text.Encoding]::UTF8.GetBytes(($body | ConvertTo-Json -Depth 8))
  Invoke-RestMethod -Method Put -Uri "$BaseUrl$path" `
    -ContentType "application/json; charset=utf-8" -Body $json -TimeoutSec $timeoutSec
}

function Extract-Json([string]$text) {
  $s = $text.IndexOf("{"); $e = $text.LastIndexOf("}")
  if ($s -lt 0 -or $e -le $s) { return $null }
  try { $text.Substring($s, $e - $s + 1) | ConvertFrom-Json } catch { $null }
}

# 等服务起来
$ready = $false
for ($i = 0; $i -lt 30; $i++) {
  try { $h = Invoke-RestMethod -Uri "$BaseUrl/api/v1/health" -TimeoutSec 3; if ($h.status -eq "ok") { $ready = $true; break } } catch {}
  Start-Sleep -Seconds 1
}
Add-Result "health" $ready "$(if ($ready) { $h.service } else { '服务未就绪' })"
if (-not $ready) { exit 1 }

try {
  & "$PSScriptRoot\api-contract-test.ps1" -BaseUrl $BaseUrl
  Add-Result "api-contract" $true "v1 路由与功能域检查通过"
} catch { Add-Result "api-contract" $false "$_" }

# 配置写入链路：应用不依赖 .env（真实来源是 SQLite/REST 写入）。
# 这里把 .env 的值当数据源，模拟前端 push / 外部平台配置。
try {
  $baseline = Invoke-RestMethod -Uri "$BaseUrl/api/v1/system/config" -TimeoutSec 10
  Add-Result "config-baseline" $true "llm=$($baseline.llmReady) img=$($baseline.imageReady) vid=$($baseline.videoReady)"
} catch { Add-Result "config-baseline" $false "$_" }

try {
  $envMap = @{}
  Get-Content "$PSScriptRoot\..\.env" | ForEach-Object {
    if ($_ -match '^\s*([A-Za-z0-9_-]+)\s*=\s*(.*)\s*$') { $envMap[$matches[1]] = $matches[2].Trim() }
  }
  $cfgBody = @{
    imageApiUrl   = $envMap["image-api-url"]; imageApiKey = $envMap["image-api-key"]; imageApiModel = $ImageModel
    videoApiUrl   = $envMap["video-api-url"]; videoApiKey = $envMap["video-api-key"]; videoApiModel = $VideoModel
    llmApiUrl     = $envMap["llm-api-url"];   llmApiKey   = $envMap["llm-api-key"];   llmApiModel   = $LlmModel
    outputDir     = ""
  }
  $r = Put-Json "/api/v1/system/config" $cfgBody 30
  $ok = $r.imageReady -and $r.llmReady -and $r.videoReady
  Add-Result "config-write" $ok "llm=$($r.llmReady) img=$($r.imageReady) vid=$($r.videoReady) source=$($r.source)"
} catch { Add-Result "config-write" $false "$_" }

try {
  $tools = Invoke-RestMethod -Uri "$BaseUrl/api/v1/catalog/tools" -TimeoutSec 10
  Add-Result "tools" ($tools.tools.Count -ge 7) "暴露 $($tools.tools.Count) 个工具"
} catch { Add-Result "tools" $false "$_" }

$director = $null; $script = $null; $anchors = $null; $shots = $null; $board = ""

if (-not $SkipText) {
  try {
    $r = Post-Json "/api/v1/agents/director/runs" @{ input = $story; model = $LlmModel } 300
    $director = $r.result
    Add-Result "director" ($director -and $director.Length -gt 50) "输出 $($director.Length) 字"
  } catch { Add-Result "director" $false "$_" }

  if ($director) {
    try {
      $r = Post-Json "/api/v1/agents/writer/runs" @{ input = $director; model = $LlmModel } 300
      $script = $r.result
      Add-Result "script" ($script -and $script.Length -gt 50) "输出 $($script.Length) 字"
    } catch { Add-Result "script" $false "$_" }
  }

  if ($script) {
    try {
      $r = Post-Json "/api/v1/agents/consistency/runs" @{ input = $script; model = $LlmModel } 300
      $anchors = $r.result
      Add-Result "consistency" ($anchors -and $anchors.Length -gt 50) "输出 $($anchors.Length) 字"
    } catch { Add-Result "consistency" $false "$_" }

    try {
      $r = Post-Json "/api/v1/agents/storyboard/runs" @{ input = $script; model = $LlmModel } 300
      $board = $r.result
      $parsed = Extract-Json $board
      if ($parsed -and $parsed.shots) { $shots = @($parsed.shots) }
      Add-Result "storyboard" ($shots -and $shots.Count -gt 0) "解析出 $(if ($shots) { $shots.Count } else { 0 }) 镜"
    } catch { Add-Result "storyboard" $false "$_" }

    try {
      $qcParts = @($script, $board, $anchors) | Where-Object { $_ }
      $qcInput = $qcParts -join "`n`n---`n`n"
      $r = Post-Json "/api/v1/agents/qc/runs" @{ input = $qcInput; model = $LlmModel } 300
      Add-Result "qc_review" ($r.result -and $r.result.Length -gt 50) "输出 $($r.result.Length) 字"
    } catch { Add-Result "qc_review" $false "$_" }
  }
}

$imagePath = $null
$imgPrompt = $null
if ($shots -and $shots.Count -gt 0) {
  $imgPrompt = $shots[0].prompt
  if (-not $imgPrompt) {
    $imgPrompt = ($shots[0] | Get-Member -MemberType NoteProperty | ForEach-Object { $shots[0].($_.Name) }) -join ", "
  }
}
if (-not $imgPrompt) {
  $imgPrompt = "深夜便利店，冷白灯光下，年轻女店员苏晚握着手机神情紧张，玻璃门外有模糊人影，写实电影感"
}

try {
  $r = Post-Json "/api/v1/media/images/generations" @{ prompt = $imgPrompt; size = "1024x1024"; model = $ImageModel } 300
  $imagePath = $r.assets[0].path
  $ok = $imagePath -and (Test-Path $imagePath) -and ((Get-Item $imagePath).Length -gt 10kb)
  Add-Result "image" $ok $imagePath
} catch { Add-Result "image" $false "$_" }

try {
  $vidBody = @{
    prompt = "便利店冷白灯光下，苏晚握着手机的手微微发抖，玻璃门外人影缓缓晃动"
    durationS = 5
    aspectRatio = "16:9"
    resolution = "720p"
    model = $VideoModel
  }
  # 新协议参考素材必须公网 URL，本地文件不传（文本生成视频）
  $r = Post-Json "/api/v1/media/videos/generations" $vidBody 600
    $videoPath = $r.assets[-1].path
  $ok = $videoPath -and (Test-Path $videoPath) -and ((Get-Item $videoPath).Length -gt 100kb)
  Add-Result "video" $ok $videoPath
} catch { Add-Result "video" $false "$_" }

Write-Host "`n===== 汇总 ====="
$steps | ForEach-Object { "{0,-12} {1}  {2}" -f $_.Step, ($(if ($_.Ok) { "PASS" } else { "FAIL" })), $_.Detail }
$failed = @($steps | Where-Object { -not $_.Ok })
if ($failed.Count -gt 0) { exit 1 } else { exit 0 }
