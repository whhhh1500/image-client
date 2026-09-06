[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$ExePath,
  [int]$CdpPort = 19439,
  [int]$ApiPort = 19440,
  [int]$MockPort = 19441,
  [ValidateSet('baseline','ai','dependency')][string]$Scenario = 'baseline',
  [switch]$KeepRunning
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not [IO.Path]::IsPathFullyQualified($ExePath)) { throw 'ExePath must be absolute' }
$sourceExe = (Resolve-Path -LiteralPath $ExePath).Path
$nodeExe = (Get-Command node -ErrorAction Stop).Source
$ports = @($CdpPort, $ApiPort, $MockPort)
if (($ports | Select-Object -Unique).Count -ne 3) { throw 'Ports must be different' }
foreach ($portNumber in $ports) {
  if ($portNumber -lt 1024 -or $portNumber -gt 65535) { throw 'Ports must be 1024..65535' }
  if (@(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object LocalPort -eq $portNumber).Count) { throw "Port $portNumber already occupied; refusing to reuse or stop it" }
}
$auditRoot = Join-Path ([IO.Path]::GetTempPath()) ('image-client-md-audit-' + [Guid]::NewGuid().ToString('N'))
$auditRoot = [IO.Path]::GetFullPath($auditRoot)
$binDir = Join-Path $auditRoot 'bin'
$dataDir = Join-Path $auditRoot 'data'
$tempDir = Join-Path $auditRoot 'tmp'
New-Item -ItemType Directory -Path $binDir, $dataDir, $tempDir | Out-Null
$copiedExe = Join-Path $binDir 'image-client.exe'
Copy-Item -LiteralPath $sourceExe -Destination $copiedExe
$sourceHash = (Get-FileHash -LiteralPath $sourceExe -Algorithm SHA256).Hash
if ((Get-FileHash -LiteralPath $copiedExe -Algorithm SHA256).Hash -ne $sourceHash) { throw 'EXE copy hash mismatch' }
$snapshot = @{
  image_api_url = "http://127.0.0.1:$MockPort/v1/images/generations"; image_api_key = 'isolated-fixture'; image_model = 'gpt-image-2'
  llm_api_url = "http://127.0.0.1:$MockPort/v1"; llm_api_key = 'isolated-fixture'; llm_model = 'gemini-3.7-flash'
  video_api_url = ''; video_api_key = ''; video_model = 'kling-video-v3'; output_dir = (Join-Path $dataDir 'assets')
}
$snapshot | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $dataDir 'backend-config.json') -Encoding utf8NoBOM
$audit = [ordered]@{ auditRoot = $auditRoot; sourceExe = $sourceExe; sha256 = $sourceHash; copiedExe = $copiedExe; dataDir = $dataDir; cdpPort = $CdpPort; apiPort = $ApiPort; mockPort = $MockPort; scenario = $Scenario; status = 'starting'; keepRunning = [bool]$KeepRunning; appPid = $null; mockPid = $null }
$driverScript = switch ($Scenario) { 'ai' { 'comic-md-ai-acceptance.mjs' } 'dependency' { 'comic-md-dependency-acceptance.mjs' } default { 'comic-md-acceptance.mjs' } }
function Write-Audit { $audit | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $auditRoot 'supervisor.json') -Encoding utf8NoBOM }
function Start-IsolatedApp {
  $startInfo = [Diagnostics.ProcessStartInfo]::new()
  $startInfo.FileName = $copiedExe
  $startInfo.WorkingDirectory = $binDir
  $startInfo.UseShellExecute = $false
  $startInfo.CreateNoWindow = $true
  $startInfo.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
  $startInfo.Environment.Clear()
  foreach ($environmentName in @('SystemRoot', 'WINDIR', 'SystemDrive', 'ComSpec', 'PATH')) { $startInfo.Environment[$environmentName] = [Environment]::GetEnvironmentVariable($environmentName, 'Process') }
  $startInfo.Environment['TEMP'] = $tempDir
  $startInfo.Environment['TMP'] = $tempDir
  $startInfo.Environment['IMAGE_CLIENT_DATA_DIR'] = $dataDir
  $startInfo.Environment['API_HOST'] = '127.0.0.1'
  $startInfo.Environment['API_PORT'] = [string]$ApiPort
  $startInfo.Environment['WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS'] = "--remote-debugging-address=127.0.0.1 --remote-debugging-port=$CdpPort"
  return [Diagnostics.Process]::Start($startInfo)
}
function Stop-OwnedProcess([Diagnostics.Process]$OwnedProcess) {
  if ($null -ne $OwnedProcess -and -not $OwnedProcess.HasExited) { $OwnedProcess.Kill($true); [void]$OwnedProcess.WaitForExit(10000) }
}
function Assert-AppOwnsPort([Diagnostics.Process]$OwnedProcess) {
  $deadline = [DateTime]::UtcNow.AddSeconds(30)
  do {
    if ($OwnedProcess.HasExited) { throw 'Isolated app exited before REST startup' }
    $listener = @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object LocalPort -eq $ApiPort)
    if ($listener.Count) {
      if (@($listener | Where-Object OwningProcess -ne $OwnedProcess.Id).Count) { throw 'REST port belongs to a foreign process' }
      return
    }
    Start-Sleep -Milliseconds 200
  } while ([DateTime]::UtcNow -lt $deadline)
  throw 'Isolated app REST port did not start'
}
$appProcess = $null; $mockProcess = $null; $success = $false
try {
  $mockStart = [Diagnostics.ProcessStartInfo]::new()
  $mockStart.FileName = $nodeExe
  $mockStart.UseShellExecute = $false
  $mockStart.CreateNoWindow = $true
  $mockStart.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
  $mockStart.ArgumentList.Add((Join-Path $PSScriptRoot 'comic-md-mock-provider.mjs'))
  $mockStart.ArgumentList.Add([string]$MockPort)
  $mockStart.ArgumentList.Add($auditRoot)
  $mockProcess = [Diagnostics.Process]::Start($mockStart)
  $audit.mockPid = $mockProcess.Id
  $appProcess = Start-IsolatedApp
  $audit.appPid = $appProcess.Id
  Write-Audit
  Write-Host "Audit root: $auditRoot`nIsolated app PID: $($appProcess.Id)"
  Assert-AppOwnsPort $appProcess
  & $nodeExe (Join-Path $PSScriptRoot $driverScript) $CdpPort $MockPort $auditRoot initial
  if ($LASTEXITCODE -ne 0) { throw 'Initial Markdown acceptance failed; inspect acceptance-initial.json' }
  Stop-OwnedProcess $appProcess
  $deadline = [DateTime]::UtcNow.AddSeconds(15)
  do {
    $remaining = @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object { $_.LocalPort -in @($CdpPort, $ApiPort) })
    if (-not $remaining.Count) { break }
    Start-Sleep -Milliseconds 250
  } while ([DateTime]::UtcNow -lt $deadline)
  if ($remaining.Count) { throw 'Own app ports not released after stop; refusing restart' }
  if ($Scenario -eq 'baseline') {
    & $nodeExe (Join-Path $PSScriptRoot 'comic-md-legacy-fixture.mjs') $auditRoot
    if ($LASTEXITCODE -ne 0) { throw 'Legacy retirement fixture seed failed' }
  }
  $appProcess = Start-IsolatedApp
  $audit.appPid = $appProcess.Id
  Write-Audit
  Assert-AppOwnsPort $appProcess
  & $nodeExe (Join-Path $PSScriptRoot $driverScript) $CdpPort $MockPort $auditRoot restore
  if ($LASTEXITCODE -ne 0) { throw 'Restart acceptance failed; inspect acceptance-restore.json' }
  $audit.status = 'passed'; $success = $true
} catch {
  $audit.status = 'failed'; $audit.error = $_.ToString(); throw
} finally {
  if (-not ($success -and $KeepRunning)) { Stop-OwnedProcess $appProcess; Stop-OwnedProcess $mockProcess }
  $audit['appRetained'] = [bool]($success -and $KeepRunning)
  Write-Audit
  Write-Host "Audit retained: $auditRoot`nApp PID: $($audit.appPid); app retained: $($audit.appRetained)"
}
