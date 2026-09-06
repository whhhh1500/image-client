[CmdletBinding()]
param(
    # Deliberately opt in twice: this script can submit up to one real image
    # request after the text/manifest gates pass.
    [switch]$Run,
    [switch]$ConfirmOneImage,
    [ValidateRange(60, 1800)]
    [int]$TimeoutSeconds = 1800,
    [ValidateNotNullOrEmpty()]
    [string]$ConfigSource = 'C:\Users\Administrator\ImageClient\backend-config.json'
)

$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$harnessBinDir = Join-Path $projectRoot 'real-e2e-harness-bin'
$harnessManifest = Join-Path $harnessBinDir 'image-client-real-e2e-harness.build-manifest.json'
$normalRelease = Join-Path $projectRoot 'src-tauri\target\release\image-client.exe'

function Assert-NoExistingReparseAncestor([Parameter(Mandatory)][string]$Path) {
    $fullPath = [System.IO.Path]::GetFullPath($Path)
    $root = [System.IO.Path]::GetPathRoot($fullPath)
    if ([string]::IsNullOrWhiteSpace($root)) {
        throw 'E2E_RUNTIME_PATH_ROOT_INVALID'
    }
    $cursor = $root
    $relative = $fullPath.Substring($root.Length)
    foreach ($part in ($relative -split '[\\/]' | Where-Object { $_ })) {
        $cursor = Join-Path $cursor $part
        if (-not ([System.IO.File]::Exists($cursor) -or [System.IO.Directory]::Exists($cursor))) {
            break
        }
        if (([System.IO.File]::GetAttributes($cursor) -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "E2E_RUNTIME_REPARSE_ANCESTOR:$cursor"
        }
    }
}

function Resolve-IsolatedRuntimeParent([Parameter(Mandatory)][string]$ProjectRoot) {
    $resolvedProjectRoot = [System.IO.Path]::GetFullPath($ProjectRoot)
    $testRoot = [System.IO.Path]::GetFullPath((Join-Path $resolvedProjectRoot '.test-tmp'))
    $runtimeParent = [System.IO.Path]::GetFullPath((Join-Path $testRoot 'image-client-real-e2e-runtime'))
    $drive = [System.IO.Path]::GetPathRoot($runtimeParent).TrimEnd([char]92, [char]47)
    if (-not $drive.Equals('D:', [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "E2E_RUNTIME_DRIVE_MUST_BE_D:$drive"
    }
    $testPrefix = $testRoot.TrimEnd([char]92, [char]47) + [System.IO.Path]::DirectorySeparatorChar
    if (-not $runtimeParent.StartsWith($testPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw 'E2E_RUNTIME_OUTSIDE_PROJECT_TEST_TMP'
    }
    Assert-NoExistingReparseAncestor $runtimeParent
    return $runtimeParent
}

function Get-Sha256([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash
}

function Confirm-HarnessCompletion([string]$Root) {
    $markerPath = Join-Path $Root 'real-e2e-rain-alley-letter.marker.json'
    $auditPath = Join-Path $Root 'real-e2e-rain-alley-letter.audit.jsonl'
    $budgetAuditPath = Join-Path $Root 'real-e2e-request-budget.audit.jsonl'
    if (-not (Test-Path -LiteralPath $markerPath -PathType Leaf) -or -not (Test-Path -LiteralPath $auditPath -PathType Leaf) -or -not (Test-Path -LiteralPath $budgetAuditPath -PathType Leaf)) {
        throw 'E2E_COMPLETION_EVIDENCE_MISSING'
    }
    $marker = Get-Content -Raw -LiteralPath $markerPath | ConvertFrom-Json
    $events = @(Get-Content -LiteralPath $auditPath | ForEach-Object { $_ | ConvertFrom-Json })
    $hasFinished = @($events | Where-Object { $_.event -eq 'finished' }).Count -eq 1
    $hasExport = @($events | Where-Object { $_.event -eq 'export_ready' }).Count -eq 1
    if ($marker.status -eq 'finished' -and $hasFinished -and $hasExport) {
        $budgetEvents = @(Get-Content -LiteralPath $budgetAuditPath | ForEach-Object { $_ | ConvertFrom-Json })
        foreach ($kind in @('source', 'adaptation', 'image')) {
            if (@($budgetEvents | Where-Object { $_.event -eq 'generation_post_reserved' -and $_.kind -eq $kind }).Count -ne 1 -or @($budgetEvents | Where-Object { $_.event -eq 'generation_http_received' -and $_.kind -eq $kind }).Count -ne 1) {
                throw 'E2E_REQUEST_BUDGET_EVIDENCE_INVALID'
            }
        }
        return
    }
    $stopped = @($events | Where-Object { $_.event -eq 'stopped' } | Select-Object -Last 1)
    $stage = 'unknown'
    $code = 'E2E_UNSAFE_STOP_CODE'
    if ($stopped.Count -eq 1) {
        if ($stopped[0].payload.stage -match '^[a-z_]+$') {
            $stage = $stopped[0].payload.stage
        }
        if ($stopped[0].payload.code -match '^E2E_[A-Z0-9_]+$') {
            $code = $stopped[0].payload.code
        }
    }
    throw "E2E_HARNESS_NOT_FINISHED_STAGE_${stage}_CODE_${code}"
}

Import-Module (Join-Path $PSScriptRoot 'real-e2e-harness-manifest.psm1') -Force
$build = Assert-RealE2eHarnessManifest -ProjectRoot $projectRoot -ManifestPath $harnessManifest
$harnessBinary = $build.BinaryPath

$harnessHash = $build.BinaryHash
$normalReleaseHash = if (Test-Path -LiteralPath $normalRelease -PathType Leaf) {
    Get-Sha256 $normalRelease
} else {
    $null
}

Write-Host "Harness binary: $harnessBinary"
Write-Host "Harness SHA-256: $harnessHash"
if ($null -ne $normalReleaseHash) {
    Write-Host "Normal release SHA-256 (read-only baseline): $normalReleaseHash"
}

if (-not $Run -or -not $ConfirmOneImage) {
    Write-Host 'Dry run only. No isolated root, configuration snapshot, app process, LLM, or image request was created.'
    Write-Host 'After review, use -Run -ConfirmOneImage to permit the gated one-page attempt.'
    exit 0
}

if (-not (Test-Path -LiteralPath $ConfigSource -PathType Leaf)) {
    throw 'The approved configuration snapshot is unavailable.'
}

$runtimeParent = Resolve-IsolatedRuntimeParent $projectRoot
New-Item -ItemType Directory -Force -Path $runtimeParent | Out-Null
Assert-NoExistingReparseAncestor $runtimeParent
$runtimeRoot = Join-Path $runtimeParent ("image-client-real-e2e-{0}" -f ([guid]::NewGuid().ToString('N')))
New-Item -ItemType Directory -Path $runtimeRoot | Out-Null
$configCopy = Join-Path $runtimeRoot 'backend-config.json'
$process = $null
$pendingError = $null
$normalReleaseChanged = $false
$configCleanup = 'not-created'
Write-Host "Isolated runtime root: $runtimeRoot"

try {
    # Copy is exact and temporary. Its contents are never parsed, written to
    # source control, or displayed by this script.
    Copy-Item -LiteralPath $ConfigSource -Destination $configCopy
    $requiredEnvironment = @('SystemRoot', 'WINDIR', 'SystemDrive', 'ComSpec', 'PATH')
    $cleanEnvironment = [ordered]@{}
    foreach ($name in $requiredEnvironment) {
        $value = [Environment]::GetEnvironmentVariable($name, 'Process')
        if ([string]::IsNullOrWhiteSpace($value)) {
            throw "Windows child-process environment is missing $name."
        }
        $cleanEnvironment[$name] = $value
    }
    $cleanEnvironment['TEMP'] = $runtimeRoot
    $cleanEnvironment['TMP'] = $runtimeRoot
    $cleanEnvironment['IMAGE_CLIENT_DATA_DIR'] = $runtimeRoot
    $cleanEnvironment['IMAGE_CLIENT_REAL_E2E_HARNESS'] = '1'

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $harnessBinary
    $startInfo.WorkingDirectory = $runtimeRoot
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Hidden
    $startInfo.Environment.Clear()
    foreach ($entry in $cleanEnvironment.GetEnumerator()) {
        $startInfo.Environment[$entry.Key] = $entry.Value
    }
    $process = [System.Diagnostics.Process]::Start($startInfo)
    if ($null -eq $process) {
        throw 'E2E_HARNESS_START_FAILED'
    }
    Write-Host "Harness PID: $($process.Id)"
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        # Do not terminate an uncertain provider call. Preserve its isolated
        # root and copied config until an operator has resolved this PID.
        throw "E2E_TIMEOUT_PID_$($process.Id)"
    }
    $process.Refresh()
    Confirm-HarnessCompletion $runtimeRoot
    if ($process.ExitCode -ne 0) {
        throw "E2E_HARNESS_EXIT_$($process.ExitCode)"
    }
    if ((Get-Sha256 $harnessBinary) -ne $harnessHash) {
        throw 'E2E_HARNESS_BINARY_CHANGED_DURING_RUN'
    }
    if ($null -ne $normalReleaseHash -and (Get-Sha256 $normalRelease) -ne $normalReleaseHash) {
        $normalReleaseChanged = $true
        throw 'NORMAL_RELEASE_CHANGED_DURING_HARNESS_RUN'
    }
    Write-Host "Completed isolated harness root: $runtimeRoot"
    Write-Host 'The marker, safe audit, generated asset, and export remain in that root for review.'
}
catch {
    $pendingError = $_
}
finally {
    $canCleanupConfig = $null -eq $process -or $process.HasExited
    if ($canCleanupConfig -and (Test-Path -LiteralPath $configCopy -PathType Leaf)) {
        # Never recurse here: remove only the exact copied credential snapshot.
        Remove-Item -LiteralPath $configCopy -Force
        $configCleanup = 'removed-exact-copy'
    }
    elseif (-not $canCleanupConfig) {
        $configCleanup = 'retained-for-live-timeout-investigation'
    }
    if ($null -ne $normalReleaseHash) {
        $normalReleaseAfter = Get-Sha256 $normalRelease
        Write-Host "Normal release SHA-256 after harness: $normalReleaseAfter"
        if ($normalReleaseAfter -ne $normalReleaseHash) {
            $normalReleaseChanged = $true
        }
    }
    Write-Host "Configuration snapshot cleanup: $configCleanup"
}

if ($normalReleaseChanged) {
    throw 'NORMAL_RELEASE_CHANGED_DURING_HARNESS_RUN'
}
if ($null -ne $pendingError) {
    throw $pendingError
}
