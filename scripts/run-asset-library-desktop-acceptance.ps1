[CmdletBinding()]
param(
  [Parameter(Mandatory)][string]$ExePath,
  [string]$ExpectedProductVersion = '0.2.3',
  [int]$CdpPort = 19539,
  [int]$ApiPort = 19540
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not [IO.Path]::IsPathRooted($ExePath)) { throw 'ExePath must be absolute' }
$sourceExe = (Resolve-Path -LiteralPath $ExePath).Path
$versionInfo = (Get-Item -LiteralPath $sourceExe).VersionInfo
$reportedVersions = @($versionInfo.FileVersion, $versionInfo.ProductVersion) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
$versionMismatches = @($reportedVersions | Where-Object { $_ -notmatch ("(?<!\\d)" + [regex]::Escape($ExpectedProductVersion) + "(?:\\.0)?(?!\\d)") })
if ($reportedVersions.Count -eq 0 -or $versionMismatches.Count -gt 0) {
  throw "EXE_PRODUCT_VERSION_MISMATCH: expected $ExpectedProductVersion; file=$($versionInfo.FileVersion); product=$($versionInfo.ProductVersion)"
}
$nodeExe = (Get-Command node -ErrorAction Stop).Source
foreach ($port in @($CdpPort, $ApiPort)) {
  if (@(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object LocalPort -eq $port).Count) { throw "Port $port already occupied; refusing to reuse or stop it" }
}
$scratchRoot = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')).Path '.test-tmp'
New-Item -ItemType Directory -Force -Path $scratchRoot | Out-Null
$auditRoot = Join-Path $scratchRoot ('image-client-asset-library-desktop-' + [Guid]::NewGuid().ToString('N'))
$binDir = Join-Path $auditRoot 'bin'; $dataDir = Join-Path $auditRoot 'data'; $tempDir = Join-Path $auditRoot 'tmp'; $fixtureDir = Join-Path $auditRoot 'fixtures'; $webViewDataDir = Join-Path $auditRoot 'webview2'
New-Item -ItemType Directory -Path $binDir, $dataDir, $tempDir, $fixtureDir, $webViewDataDir | Out-Null
$copiedExe = Join-Path $binDir 'image-client.exe'
Copy-Item -LiteralPath $sourceExe -Destination $copiedExe
$sourceHash = (Get-FileHash -LiteralPath $sourceExe -Algorithm SHA256).Hash
if ((Get-FileHash -LiteralPath $copiedExe -Algorithm SHA256).Hash -ne $sourceHash) { throw 'EXE_COPY_HASH_MISMATCH' }
$fixturePng = Join-Path $fixtureDir 'external-upload-fixture.png'
Copy-Item -LiteralPath (Join-Path $PSScriptRoot '..\public\app-icon.png') -Destination $fixturePng
$fixtureMp4 = Join-Path $fixtureDir 'external-upload-fixture.mp4'
$ffmpeg = (Get-Command ffmpeg -ErrorAction Stop).Source
& $ffmpeg -hide_banner -loglevel error -y -f lavfi -i 'color=c=0x2563eb:s=64x64:d=1' -an -c:v libx264 -pix_fmt yuv420p -movflags +faststart $fixtureMp4
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $fixtureMp4)) { throw 'FIXTURE_MP4_CREATE_FAILED' }
$audit = [ordered]@{ auditRoot = $auditRoot; sourceExe = $sourceExe; sha256 = $sourceHash; fileVersion = $versionInfo.FileVersion; productVersion = $versionInfo.ProductVersion; dataDir = $dataDir; webViewDataDir = $webViewDataDir; fixturePng = $fixturePng; fixtureMp4 = $fixtureMp4; cdpPort = $CdpPort; apiPort = $ApiPort; status = 'starting'; phase = $null; appPid = $null }
function Write-Audit { $audit | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $auditRoot 'supervisor.json') -Encoding UTF8 }
function Start-IsolatedApp {
  $startInfo = [Diagnostics.ProcessStartInfo]::new()
  $startInfo.FileName = $copiedExe; $startInfo.WorkingDirectory = $binDir; $startInfo.UseShellExecute = $false; $startInfo.CreateNoWindow = $true; $startInfo.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
  $startInfo.Environment.Clear()
  foreach ($name in @('SystemRoot', 'WINDIR', 'SystemDrive', 'ComSpec', 'PATH')) { $startInfo.Environment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
  $startInfo.Environment['TEMP'] = $tempDir; $startInfo.Environment['TMP'] = $tempDir; $startInfo.Environment['IMAGE_CLIENT_DATA_DIR'] = $dataDir; $startInfo.Environment['WEBVIEW2_USER_DATA_FOLDER'] = $webViewDataDir
  $startInfo.Environment['API_HOST'] = '127.0.0.1'; $startInfo.Environment['API_PORT'] = [string]$ApiPort
  $startInfo.Environment['WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS'] = "--remote-debugging-address=127.0.0.1 --remote-debugging-port=$CdpPort"
  foreach ($name in @('IMAGE_API_KEY','LLM_API_KEY','VIDEO_API_KEY','OPENAI_API_KEY','ANTHROPIC_API_KEY')) { $startInfo.Environment[$name] = '' }
  return [Diagnostics.Process]::Start($startInfo)
}
function Stop-OwnedProcess([Diagnostics.Process]$Process) { if ($null -ne $Process -and -not $Process.HasExited) { $Process.Kill(); [void]$Process.WaitForExit(10000) } }
function Wait-OwnedPort([Diagnostics.Process]$Process) {
  $deadline = [DateTime]::UtcNow.AddSeconds(30)
  while ([DateTime]::UtcNow -lt $deadline) {
    if ($Process.HasExited) { throw 'ISOLATED_APP_EXITED_BEFORE_REST_STARTUP' }
    $listeners = @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object LocalPort -eq $ApiPort)
    if ($listeners.Count -eq 1 -and $listeners[0].OwningProcess -eq $Process.Id) { return }
    if ($listeners.Count) { throw 'REST_PORT_OWNED_BY_FOREIGN_PROCESS' }
    Start-Sleep -Milliseconds 200
  }
  throw 'ISOLATED_APP_REST_PORT_TIMEOUT'
}
$process = $null
try {
  foreach ($phase in @('initial', 'restore')) {
    $audit.phase = $phase; $process = Start-IsolatedApp; $audit.appPid = $process.Id; Write-Audit
    Wait-OwnedPort $process
    & $nodeExe (Join-Path $PSScriptRoot 'asset-library-desktop-acceptance.mjs') $CdpPort $auditRoot $phase
    if ($LASTEXITCODE -ne 0) { throw "ASSET_LIBRARY_DESKTOP_${phase}_FAILED" }
    Stop-OwnedProcess $process; $process = $null; $audit.appPid = $null; Write-Audit
    Start-Sleep -Milliseconds 500
  }
  $audit.status = 'passed'
} catch { $audit.status = 'failed'; $audit.error = $_.ToString(); throw } finally { Stop-OwnedProcess $process; $audit.appPid = $null; Write-Audit; Write-Host "Audit retained: $auditRoot" }
