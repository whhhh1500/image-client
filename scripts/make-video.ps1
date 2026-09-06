# 生成多段视频用于拼接验证：POST /api/v1/media/videos/generations 后打印分段路径清单。
param(
  [int]$Seconds = 20,
  [string]$VideoModel = "grok-imagine-video-1.5-preview",
  [string]$OutList = ""
)
$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$isWindowsPlatform = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)
$binary = if ($isWindowsPlatform) { "image-client.exe" } else { "image-client" }
$exePath = Join-Path $repoRoot "src-tauri/target/release/$binary"
if (-not (Test-Path -LiteralPath $exePath)) { throw "可执行文件不存在: $exePath" }

$startArgs = @{ FilePath = $exePath; WorkingDirectory = (Split-Path $exePath); PassThru = $true }
if ($isWindowsPlatform) { $startArgs.WindowStyle = "Hidden" }
$app = Start-Process @startArgs
try {
  $base = "http://127.0.0.1:8123"
  for ($i = 0; $i -lt 30; $i++) {
    try { $h = Invoke-RestMethod "$base/api/v1/health" -TimeoutSec 2; if ($h.status -eq "ok") { break } } catch {}
    Start-Sleep 1
  }
  & (Join-Path $PSScriptRoot "apply-config.ps1") -BaseUrl $base -VideoModel $VideoModel | Out-Null
  $body = @{
    prompt = "雨夜城市街道，重型泥头车冲下陡坡驶向斑马线，车灯撕裂雨幕，电影感慢镜头，霓虹灯倒影"
    duration_s = $Seconds
    model = $VideoModel
  } | ConvertTo-Json
  $r = Invoke-RestMethod -Method Post -Uri "$base/api/v1/media/videos/generations" `
    -ContentType "application/json; charset=utf-8" `
    -Body ([System.Text.Encoding]::UTF8.GetBytes($body)) -TimeoutSec 900
  $paths = @($r.assets | ForEach-Object { $_.path })
  $paths | ForEach-Object { Write-Host "seg: $_" }
  if ($OutList) { $paths | Set-Content -Path $OutList -Encoding UTF8 }
} finally {
  Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue
}
