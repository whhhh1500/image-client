# 发布包冒烟：启动 release executable → health/tools/config → 关闭。
param(
  [string]$ExePath = "",
  [int]$Port = 18123,
  [switch]$TestHistorySync,
  [switch]$UseIsolatedHome
)

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$isWindowsPlatform = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)
if (-not $ExePath) {
  $binary = if ($isWindowsPlatform) { "image-client.exe" } else { "image-client" }
  $ExePath = Join-Path $repoRoot "src-tauri/target/release/$binary"
}
if (-not (Test-Path -LiteralPath $ExePath)) { throw "可执行文件不存在: $ExePath" }

$isolatedHome = $null
if ($UseIsolatedHome) {
  $isolatedHome = [System.IO.Path]::GetFullPath((Join-Path $repoRoot ".smoke-runtime-$PID-$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())"))
  $workspacePrefix = $repoRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
  if (-not $isolatedHome.StartsWith($workspacePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "隔离测试目录越出工作区: $isolatedHome"
  }
  New-Item -ItemType Directory -Path $isolatedHome -Force | Out-Null
}

$processEnvironment = @{ API_PORT = [string]$Port; API_HOST = "127.0.0.1" }
if ($isolatedHome) {
  $processEnvironment.USERPROFILE = $isolatedHome
  $processEnvironment.HOME = $isolatedHome
}

$startArgs = @{
  FilePath = $ExePath
  WorkingDirectory = (Split-Path $ExePath)
  PassThru = $true
  Environment = $processEnvironment
}
if ($isWindowsPlatform) { $startArgs.WindowStyle = "Hidden" }
$p = Start-Process @startArgs
try {
  $baseUrl = "http://127.0.0.1:$Port"
  $ok = $false
  for ($i = 0; $i -lt 30; $i++) {
    try { $h = Invoke-RestMethod "$baseUrl/api/v1/health" -TimeoutSec 2; if ($h.status -eq "ok") { $ok = $true; break } } catch {}
    Start-Sleep 1
  }
  Write-Host "health: $ok"
  $t = Invoke-RestMethod "$baseUrl/api/v1/catalog/tools" -TimeoutSec 10
  Write-Host "tools: $($t.tools.Count)"
  $c = Invoke-RestMethod "$baseUrl/api/v1/system/config" -TimeoutSec 10
  & "$PSScriptRoot\api-contract-test.ps1" -BaseUrl $baseUrl
  if ($TestHistorySync) {
    if (-not $isolatedHome) { throw "TestHistorySync 必须与 UseIsolatedHome 一起使用，避免污染正式历史" }
    $listenerReady = $false
    for ($i = 0; $i -lt 120; $i++) {
      try {
        $sync = Invoke-RestMethod "$baseUrl/api/v1/system/history-sync" -TimeoutSec 2
        if ($sync.frontendListenerReady) { $listenerReady = $true; break }
      } catch {}
      Start-Sleep -Milliseconds 250
    }
    if (-not $listenerReady) { throw "前台历史事件监听器未就绪" }
    $marker = "history-sync-$([Guid]::NewGuid().ToString('N'))"
    $body = @{ label = "接口同步测试"; text = $marker; projectId = "smoke-project" } | ConvertTo-Json -Compress
    $saved = Invoke-RestMethod -Method Post -Uri "$baseUrl/api/v1/assets/text" -ContentType "application/json" -Body $body -TimeoutSec 10
    if (-not $saved.asset.id -or -not (Test-Path -LiteralPath $saved.asset.path)) { throw "外部接口没有返回已落盘资产" }
    $eventAcknowledged = $false
    for ($i = 0; $i -lt 40; $i++) {
      try {
        $sync = Invoke-RestMethod "$baseUrl/api/v1/system/history-sync" -TimeoutSec 2
        if ($sync.emittedRevision -gt 0 -and $sync.acknowledgedRevision -ge $sync.emittedRevision -and $sync.pendingRevision -eq 0) {
          $eventAcknowledged = $true
          break
        }
      } catch {}
      Start-Sleep -Milliseconds 250
    }
    if (-not $eventAcknowledged) { throw "外部接口成功后，前台没有确认历史刷新事件" }
    Write-Host "history sync: REST -> SQLite -> event -> UI PASS"
  }
  Write-Host "config: llm=$($c.llmReady) img=$($c.imageReady) vid=$($c.videoReady) source=$($c.source)"
  if ($ok -and $t.tools.Count -ge 7) { exit 0 } else { exit 1 }
} finally {
  Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
  Wait-Process -Id $p.Id -Timeout 10 -ErrorAction SilentlyContinue
  if ($isolatedHome -and (Test-Path -LiteralPath $isolatedHome)) {
    $resolvedHome = [System.IO.Path]::GetFullPath($isolatedHome)
    $workspacePrefix = $repoRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
    if ($resolvedHome.StartsWith($workspacePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
      for ($i = 0; $i -lt 20 -and (Test-Path -LiteralPath $resolvedHome); $i++) {
        Remove-Item -LiteralPath $resolvedHome -Recurse -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $resolvedHome) { Start-Sleep -Milliseconds 250 }
      }
      if (Test-Path -LiteralPath $resolvedHome) { Write-Warning "隔离测试目录仍被占用，保留以便检查: $resolvedHome" }
    }
  }
}
