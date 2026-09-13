# Isolated single-EXE release smoke. This script only calls local read-only GET
# endpoints and deliberately leaves its temporary audit root for review.
[CmdletBinding()]
param(
  [Parameter(Mandatory)]
  [ValidateNotNullOrEmpty()]
  [string]$ExePath,

  [ValidateRange(1, 65535)]
  [int]$Port = 18123
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-Sha256 {
  param([Parameter(Mandatory)][string]$Path)

  return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToUpperInvariant()
}

function Get-ListeningPortOwners {
  param([Parameter(Mandatory)][int]$LocalPort)

  $owners = @(Get-NetTCPConnection -State Listen -ErrorAction Stop |
      Where-Object { $_.LocalPort -eq $LocalPort } |
      Select-Object LocalAddress, LocalPort, OwningProcess)
  # Callers deliberately wrap this in @(...), so no-listener and one-listener
  # cases stay StrictMode-safe without nested arrays.
  return $owners
}

function Get-ProcessTreeIds {
  param([Parameter(Mandatory)][int]$RootProcessId)

  $seen = [System.Collections.Generic.HashSet[int]]::new()
  $pending = [System.Collections.Generic.Queue[int]]::new()
  $pending.Enqueue($RootProcessId)
  while ($pending.Count -gt 0) {
    $parentId = $pending.Dequeue()
    if (-not $seen.Add($parentId)) { continue }
    $children = @(Get-CimInstance -ClassName Win32_Process -Filter "ParentProcessId = $parentId" -ErrorAction Stop)
    foreach ($child in $children) {
      $pending.Enqueue([int]$child.ProcessId)
    }
  }
  return @($seen)
}

function Test-ExactPath {
  param(
    [Parameter(Mandatory)][string]$Actual,
    [Parameter(Mandatory)][string]$Expected
  )

  $actualFull = [System.IO.Path]::GetFullPath($Actual).TrimEnd([char]92, [char]47)
  $expectedFull = [System.IO.Path]::GetFullPath($Expected).TrimEnd([char]92, [char]47)
  return $actualFull.Equals($expectedFull, [System.StringComparison]::OrdinalIgnoreCase)
}

function Write-Audit {
  param(
    [Parameter(Mandatory)][System.Collections.IDictionary]$Audit,
    [Parameter(Mandatory)][string]$AuditPath
  )

  $Audit["updatedAtUtc"] = [DateTimeOffset]::UtcNow.ToString("O")
  $Audit | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $AuditPath -Encoding utf8
}

if (-not [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)) {
  throw "此隔离 release smoke 仅支持 Windows WebView2。"
}
if (-not [System.IO.Path]::IsPathFullyQualified($ExePath)) {
  throw "ExePath 必须是绝对路径。"
}
if (-not (Test-Path -LiteralPath $ExePath -PathType Leaf)) {
  throw "可执行文件不存在: $ExePath"
}
if ([System.IO.Path]::GetExtension($ExePath) -ne ".exe") {
  throw "ExePath 必须指向 .exe 文件。"
}

$resolvedExe = (Resolve-Path -LiteralPath $ExePath).Path
$preflightListeners = @(Get-ListeningPortOwners -LocalPort $Port)
if ($preflightListeners.Count -gt 0) {
  throw "端口 $Port 已被监听，拒绝借用或停止现有进程。"
}

# 审计产物留在仓库所在的盘（.test-tmp），不要把几 GB 的临时目录写进系统盘。
$tempBase = [System.IO.Path]::GetFullPath((Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')).Path '.test-tmp')).TrimEnd([char]92, [char]47)
New-Item -ItemType Directory -Force -Path $tempBase | Out-Null
$auditRoot = [System.IO.Path]::GetFullPath((Join-Path $tempBase ("image-client-exe-audit-" + [Guid]::NewGuid().ToString("N"))))
$auditPrefix = (Join-Path $tempBase "image-client-exe-audit-")
if (-not $auditRoot.StartsWith($auditPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
  throw "拒绝在审计根目录外创建临时目录: $auditRoot"
}

$binDir = Join-Path $auditRoot "bin"
$dataDir = Join-Path $auditRoot "data"
$tempDir = Join-Path $auditRoot "tmp"
$copiedExe = Join-Path $binDir "image-client.exe"
$auditPath = Join-Path $auditRoot "audit.json"
New-Item -ItemType Directory -Path $binDir, $dataDir, $tempDir -Force | Out-Null
Write-Host "审计目录（将保留）：$auditRoot" -ForegroundColor Cyan

$sourceHash = Get-Sha256 -Path $resolvedExe
$audit = [ordered]@{
  schemaVersion = 1
  status = "preparing"
  startedAtUtc = [DateTimeOffset]::UtcNow.ToString("O")
  executable = [ordered]@{
    sourcePath = $resolvedExe
    sha256 = $sourceHash
    copiedPath = $copiedExe
    copySha256 = $null
  }
  pid = $null
  port = $Port
  paths = [ordered]@{
    auditRoot = $auditRoot
    dataRoot = $dataDir
    expectedAssets = (Join-Path $dataDir "assets")
    expectedLogs = (Join-Path $dataDir "logs")
    expectedProfileRoot = (Join-Path $dataDir "webview2")
    configOutputDir = $null
    configSource = $null
    logsRouteDirectory = $null
  }
  checks = [ordered]@{
    preflightPortUnused = $true
    executableHashMatched = $false
    listenerOwnedByStartedProcess = $false
    apiHealth = $false
    configRoute = $false
    logsRoute = $false
    historySyncFrontendListener = $false
    isolatedRustData = $false
    webViewProfileHasFiles = $false
    frontendReadyProxy = $false
    nonWhiteScreenVerified = $false
  }
  cleanup = [ordered]@{
    forcedTreeTermination = $false
    capturedChildPids = @()
    remainingCapturedPids = @()
    portReleased = $false
    gracefulShutdownVerified = $false
  }
  notes = @(
    "仅调用本地 GET /api/v1/health、system/config、system/logs、system/history-sync。",
    "history-sync frontendListenerReady 只是前端事件监听就绪代理，不是非白屏截图或 DOM 验证。",
    "finally 使用 Kill(true) 强制终止自启进程树；这不是 graceful shutdown 验证。",
    "审计根故意保留，脚本不会递归删除它。"
  )
}

$process = $null
$capturedProcessIds = [System.Collections.Generic.HashSet[int]]::new()
$cleanupMustFail = $false
try {
  Copy-Item -LiteralPath $resolvedExe -Destination $copiedExe -Force
  $copyHash = Get-Sha256 -Path $copiedExe
  $audit.executable.copySha256 = $copyHash
  if ($copyHash -ne $sourceHash) {
    throw "复制后的 EXE SHA-256 与源文件不一致。"
  }
  $audit.checks.executableHashMatched = $true

  $emptyBackendConfig = [ordered]@{
    image_api_url = ""
    image_api_key = ""
    image_model = "gpt-image-2"
    video_api_url = ""
    video_api_key = ""
    video_model = "kling-video-v3"
    llm_api_url = ""
    llm_api_key = ""
    llm_model = "gemini-3.7-flash"
    output_dir = '$DEFAULT_ASSETS'
  }
  $emptyBackendConfig | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $dataDir "backend-config.json") -Encoding utf8
  Write-Audit -Audit $audit -AuditPath $auditPath

  $requiredEnvironment = @("SystemRoot", "WINDIR", "SystemDrive", "ComSpec", "PATH")
  $cleanEnvironment = [ordered]@{}
  foreach ($name in $requiredEnvironment) {
    $value = [Environment]::GetEnvironmentVariable($name, "Process")
    if ([string]::IsNullOrWhiteSpace($value)) {
      throw "当前进程缺少启动 Windows 子进程所需的 $name 环境变量。"
    }
    $cleanEnvironment[$name] = $value
  }
  $cleanEnvironment["TEMP"] = $tempDir
  $cleanEnvironment["TMP"] = $tempDir
  $cleanEnvironment["IMAGE_CLIENT_DATA_DIR"] = $dataDir
  $cleanEnvironment["API_HOST"] = "127.0.0.1"
  $cleanEnvironment["API_PORT"] = [string]$Port

  $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
  $startInfo.FileName = $copiedExe
  $startInfo.WorkingDirectory = $binDir
  $startInfo.UseShellExecute = $false
  $startInfo.CreateNoWindow = $true
  $startInfo.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Hidden
  $startInfo.Environment.Clear()
  foreach ($entry in $cleanEnvironment.GetEnumerator()) {
    $startInfo.Environment[$entry.Key] = $entry.Value
  }

  $process = [System.Diagnostics.Process]::Start($startInfo)
  if ($null -eq $process) {
    throw "无法启动隔离 release EXE。"
  }
  $audit.pid = $process.Id
  foreach ($capturedId in @(Get-ProcessTreeIds -RootProcessId $process.Id)) {
    [void]$capturedProcessIds.Add([int]$capturedId)
  }

  $baseUrl = "http://127.0.0.1:$Port"
  $deadline = [DateTimeOffset]::UtcNow.AddSeconds(45)
  $healthReady = $false
  $configurationChecked = $false
  $configurationRecheckedAfterFrontendReady = $false
  $profileChecked = $false
  while ([DateTimeOffset]::UtcNow -lt $deadline) {
    if ($process.HasExited) {
      throw "隔离 release EXE 在 health 就绪前退出。"
    }
    foreach ($capturedId in @(Get-ProcessTreeIds -RootProcessId $process.Id)) {
      [void]$capturedProcessIds.Add([int]$capturedId)
    }

    $listeners = @(Get-ListeningPortOwners -LocalPort $Port)
    if ($listeners.Count -gt 0) {
      if ($listeners.Count -ne 1 -or [int]$listeners[0].OwningProcess -ne $process.Id) {
        throw "端口 $Port 在读取 API 前由非自启进程监听。"
      }
      $audit.checks.listenerOwnedByStartedProcess = $true

      try {
        $health = Invoke-RestMethod -Method Get -Uri "$baseUrl/api/v1/health" -TimeoutSec 2
        if ($health.status -eq "ok" -and $health.apiVersion -eq "v1") {
          $healthReady = $true
          $audit.checks.apiHealth = $true
        }
      } catch {
        Start-Sleep -Seconds 1
        continue
      }

      if ($healthReady -and -not $configurationChecked) {
        $config = Invoke-RestMethod -Method Get -Uri "$baseUrl/api/v1/system/config" -TimeoutSec 3
        $logs = Invoke-RestMethod -Method Get -Uri "$baseUrl/api/v1/system/logs" -TimeoutSec 3
        if ($config.imageReady -or $config.videoReady -or $config.llmReady -or
            $config.source -notin @("none", "db")) {
          throw "隔离配置不是无 provider 的空配置。"
        }
        if (-not (Test-ExactPath -Actual $config.outputDir -Expected (Join-Path $dataDir "assets"))) {
          throw "配置接口返回的输出目录未落在隔离 data root。"
        }
        if (-not (Test-ExactPath -Actual $logs.directory -Expected (Join-Path $dataDir "logs"))) {
          throw "日志接口返回的目录未落在隔离 data root。"
        }
        $audit.paths.configOutputDir = $config.outputDir
        $audit.paths.configSource = $config.source
        $audit.paths.logsRouteDirectory = $logs.directory
        $audit.checks.configRoute = $true
        $audit.checks.logsRoute = $true
        $configurationChecked = $true
      }

      if ($configurationChecked) {
        $sync = Invoke-RestMethod -Method Get -Uri "$baseUrl/api/v1/system/history-sync" -TimeoutSec 3
        if ($sync.mode -ne "event_driven" -or $sync.polling) {
          throw "history-sync 接口没有返回预期的事件驱动状态。"
        }
        if ($sync.frontendListenerReady) {
          $audit.checks.historySyncFrontendListener = $true
          $audit.checks.frontendReadyProxy = $true
        }

        # The React bootstrap may persist its empty settings after the first
        # config read, changing source none -> db. Re-read only after that
        # frontend listener proxy is ready and keep provider readiness strict.
        if ($sync.frontendListenerReady -and -not $configurationRecheckedAfterFrontendReady) {
          $finalConfig = Invoke-RestMethod -Method Get -Uri "$baseUrl/api/v1/system/config" -TimeoutSec 3
          if ($finalConfig.imageReady -or $finalConfig.videoReady -or $finalConfig.llmReady -or
              $finalConfig.source -notin @("none", "db")) {
            throw "前端初始化后的隔离配置不是无 provider 的空配置。"
          }
          if (-not (Test-ExactPath -Actual $finalConfig.outputDir -Expected (Join-Path $dataDir "assets"))) {
            throw "前端初始化后配置接口返回的输出目录未落在隔离 data root。"
          }
          $audit.paths.configOutputDir = $finalConfig.outputDir
          $audit.paths.configSource = $finalConfig.source
          $configurationRecheckedAfterFrontendReady = $true
        }

        $profileRoot = Join-Path $dataDir "webview2"
        $profileFiles = @()
        if (Test-Path -LiteralPath $profileRoot -PathType Container) {
          $profileFiles = @(Get-ChildItem -LiteralPath $profileRoot -Force -Recurse -File)
        }
        if ($profileFiles.Count -gt 0) {
          $audit.checks.webViewProfileHasFiles = $true
          $profileChecked = $true
        }
        if ((Test-Path -LiteralPath (Join-Path $dataDir "image-client.db") -PathType Leaf) -and
            (Test-Path -LiteralPath (Join-Path $dataDir "assets") -PathType Container) -and
            (Test-Path -LiteralPath (Join-Path $dataDir "logs") -PathType Container)) {
          $audit.checks.isolatedRustData = $true
        }

        if ($audit.checks.frontendReadyProxy -and $configurationRecheckedAfterFrontendReady -and
            $profileChecked -and $audit.checks.isolatedRustData) {
          break
        }
      }
    }
    Start-Sleep -Seconds 1
  }

  if (-not $healthReady) { throw "45 秒内未获得隔离 API health。" }
  if (-not $configurationChecked) { throw "未完成隔离 config/logs 路由核验。" }
  if (-not $audit.checks.frontendReadyProxy) { throw "45 秒内前端 history-sync listener 未就绪。" }
  if (-not $configurationRecheckedAfterFrontendReady) { throw "未完成前端初始化后的隔离配置复核。" }
  if (-not $audit.checks.webViewProfileHasFiles) { throw "隔离 WebView2 profile 没有实际文件，目录存在不算通过。" }
  if (-not $audit.checks.isolatedRustData) { throw "隔离 data root 缺少数据库、assets 或 logs。" }

  $audit.status = "passed_api_and_frontend_proxy"
  Write-Host "PASS isolated release: API health + frontend listener proxy + isolated profile files." -ForegroundColor Green
  Write-Host "未验证：非白屏 DOM/截图；关闭将强制终止，非 graceful shutdown。" -ForegroundColor Yellow
} catch {
  $audit.status = "failed"
  $audit.checks.failureRecorded = $true
  throw
} finally {
  if ($null -ne $process) {
    try {
      foreach ($capturedId in @(Get-ProcessTreeIds -RootProcessId $process.Id)) {
        [void]$capturedProcessIds.Add([int]$capturedId)
      }
    } catch {
      $audit.cleanup.processTreeCaptureFailed = $true
    }

    try {
      if (-not $process.HasExited) {
        $process.Kill($true)
        $audit.cleanup.forcedTreeTermination = $true
      }
      $process.WaitForExit(10000) | Out-Null
    } catch {
      $audit.cleanup.processTerminationFailed = $true
    }
  }

  # Kill(true) is asynchronous for descendants. Wait only for PIDs captured
  # from this Process tree; never search for or stop unrelated processes.
  $childExitDeadline = [DateTimeOffset]::UtcNow.AddSeconds(5)
  do {
    $remainingCaptured = @(
      $capturedProcessIds | Where-Object {
        $candidateId = $_
        $null -ne (Get-Process -Id $candidateId -ErrorAction SilentlyContinue)
      }
    )
    if ($remainingCaptured.Count -eq 0 -or [DateTimeOffset]::UtcNow -ge $childExitDeadline) {
      break
    }
    Start-Sleep -Milliseconds 200
  } while ($true)

  $audit.cleanup.capturedChildPids = @($capturedProcessIds | Where-Object { $_ -ne $audit.pid } | Sort-Object -Unique)
  $audit.cleanup.remainingCapturedPids = @($remainingCaptured | Sort-Object -Unique)
  try {
    $audit.cleanup.portReleased = @(Get-ListeningPortOwners -LocalPort $Port).Count -eq 0
  } catch {
    $audit.cleanup.portReleaseCheckFailed = $true
  }
  $audit.endedAtUtc = [DateTimeOffset]::UtcNow.ToString("O")
  if ($audit.status -eq "passed_api_and_frontend_proxy" -and
      ($audit.cleanup.remainingCapturedPids.Count -gt 0 -or -not $audit.cleanup.portReleased)) {
    $audit.status = "cleanup_failed"
    $cleanupMustFail = $true
  }
  Write-Audit -Audit $audit -AuditPath $auditPath
  if ($cleanupMustFail) {
    throw "隔离 smoke 的自启进程树或端口未完全退出；保留 audit 供复核。"
  }
}
