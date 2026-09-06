[CmdletBinding()]
param(
    [Parameter(ParameterSetName='Release', Mandatory)]
    [string]$RuntimeRoot,
    [Parameter(ParameterSetName='Release', Mandatory)]
    [string]$CheckpointPath,
    [Parameter(ParameterSetName='Release', Mandatory)]
    [string]$JobId,
    [Parameter(ParameterSetName='Release', Mandatory)]
    [string]$ManifestId,
    [Parameter(ParameterSetName='Release', Mandatory)]
    [string]$ManifestFingerprint,
    [Parameter(ParameterSetName='Release', Mandatory)]
    [string]$RequestDigest,
    [Parameter(ParameterSetName='Release')]
    [string]$PythonPath = 'python',
    [Parameter(ParameterSetName='SelfTest', Mandatory)]
    [switch]$SelfTest,
    [Parameter(ParameterSetName='ExclusiveWriteSelfTest', Mandatory)]
    [switch]$ExclusiveWriteSelfTest,
    [Parameter(ParameterSetName='ExclusiveWriteSelfTest', Mandatory)]
    [string]$TestPath
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$testParent = [IO.Path]::GetFullPath((Join-Path $projectRoot '.test-tmp')).TrimEnd('\','/') + [IO.Path]::DirectorySeparatorChar
$dbGate = Join-Path $PSScriptRoot 'db_gate.py'

function Assert-NoExistingReparseAncestor([Parameter(Mandatory)][string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    $root = [IO.Path]::GetPathRoot($full)
    if ([string]::IsNullOrWhiteSpace($root)) { throw 'NORMAL_UI_REVIEW_PATH_ROOT_INVALID' }
    $cursor = $root
    foreach ($part in ($full.Substring($root.Length) -split '[\\/]' | Where-Object { $_ })) {
        $cursor = Join-Path $cursor $part
        if (-not ([IO.File]::Exists($cursor) -or [IO.Directory]::Exists($cursor))) { break }
        if (([IO.File]::GetAttributes($cursor) -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'NORMAL_UI_REVIEW_REPARSE_ANCESTOR' }
    }
}

function Resolve-ReviewRuntime([Parameter(Mandatory)][string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    if (-not $full.StartsWith($testParent, [StringComparison]::OrdinalIgnoreCase) -or -not $full.StartsWith('D:\', [StringComparison]::OrdinalIgnoreCase)) { throw 'NORMAL_UI_REVIEW_OUTSIDE_D_TEST_TMP' }
    Assert-NoExistingReparseAncestor $full
    return $full
}

function Read-ObjectJson([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Code) {
    try {
        $value = Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json
        if ($null -eq $value -or $value -isnot [psobject]) { throw 'invalid' }
        return $value
    } catch { throw $Code }
}

function Assert-NonEmptyString([object]$Value, [string]$Code) {
    if ([string]::IsNullOrWhiteSpace([string]$Value)) { throw $Code }
}

function Write-AtomicJson([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][System.Collections.IDictionary]$Value) {
    $temporary = "$Path.$([Guid]::NewGuid().ToString('N')).tmp"
    try {
        [IO.File]::WriteAllText($temporary, (ConvertTo-Json -Compress $Value))
        [IO.File]::Move($temporary, $Path, $true)
    } finally {
        if (Test-Path -LiteralPath $temporary -PathType Leaf) { Remove-Item -LiteralPath $temporary -Force }
    }
}

function Write-ExclusiveJson([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][System.Collections.IDictionary]$Value) {
    $created = $false
    $stream = $null
    try {
        try {
            $stream = [IO.FileStream]::new($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            $created = $true
        } catch [IO.IOException] {
            throw 'NORMAL_UI_REVIEW_RELEASE_ALREADY_EXISTS'
        }
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes((ConvertTo-Json -Compress $Value))
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
    } catch {
        if ($created -and $null -ne $stream) {
            try { $stream.Dispose(); $stream = $null; Remove-Item -LiteralPath $Path -Force -ErrorAction Stop } catch { }
        }
        throw
    } finally {
        if ($null -ne $stream) { $stream.Dispose() }
    }
}

function Test-CheckpointExact([Parameter(Mandatory)][psobject]$Checkpoint, [Parameter(Mandatory)][hashtable]$Expected) {
    if ($Checkpoint.schemaVersion -ne 'normal-ui-image-review.v1' -or $Checkpoint.formalManifestReview -isnot [psobject]) { return $false }
    foreach ($pair in $Expected.GetEnumerator()) {
        if ([string]$Checkpoint.($pair.Key) -ne [string]$pair.Value) { return $false }
    }
    return $true
}

if ($ExclusiveWriteSelfTest) {
    Write-ExclusiveJson $TestPath ([ordered]@{ marker='exclusive' })
    Write-Output '{"event":"normal_ui_review_exclusive_write_succeeded"}'
    return
}

if ($SelfTest) {
    $checkpoint = [pscustomobject]@{ schemaVersion='normal-ui-image-review.v1'; jobId='job'; manifestId='manifest'; manifestFingerprint='fingerprint'; requestDigest='sha256:abc'; formalManifestReview=[pscustomobject]@{ pageNo=1 } }
    $expected = @{ jobId='job'; manifestId='manifest'; manifestFingerprint='fingerprint'; requestDigest='sha256:abc' }
    if (-not (Test-CheckpointExact $checkpoint $expected)) { throw 'NORMAL_UI_REVIEW_SELFTEST_EXACT_FAILED' }
    $expected.requestDigest = 'sha256:different'
    if (Test-CheckpointExact $checkpoint $expected) { throw 'NORMAL_UI_REVIEW_SELFTEST_MISMATCH_ACCEPTED' }
    $outsideRejected = $false
    try { Resolve-ReviewRuntime 'C:\outside' | Out-Null } catch { $outsideRejected = $_.Exception.Message -eq 'NORMAL_UI_REVIEW_OUTSIDE_D_TEST_TMP' }
    if (-not $outsideRejected) { throw 'NORMAL_UI_REVIEW_SELFTEST_OUTSIDE_ACCEPTED' }
    $selfTestRoot = Join-Path $testParent ('review-release-selftest-' + [Guid]::NewGuid().ToString('N'))
    try {
        New-Item -ItemType Directory -Path $selfTestRoot | Out-Null
        $exclusivePath = Join-Path $selfTestRoot 'release.json'
        $processes = @()
        foreach ($index in 1..2) {
            $processes += Start-Process -FilePath 'pwsh' -ArgumentList @('-NoProfile','-File',$PSCommandPath,'-ExclusiveWriteSelfTest','-TestPath',$exclusivePath) -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $selfTestRoot "worker-$index.out.log") -RedirectStandardError (Join-Path $selfTestRoot "worker-$index.err.log")
        }
        foreach ($process in $processes) { $process.WaitForExit() }
        if (@($processes | Where-Object { $_.ExitCode -eq 0 }).Count -ne 1 -or @($processes | Where-Object { $_.ExitCode -ne 0 }).Count -ne 1) { throw 'NORMAL_UI_REVIEW_SELFTEST_EXCLUSIVE_RACE_FAILED' }
        $exclusive = Get-Content -Raw -LiteralPath $exclusivePath | ConvertFrom-Json
        if ($exclusive.marker -ne 'exclusive') { throw 'NORMAL_UI_REVIEW_SELFTEST_EXCLUSIVE_CONTENT_FAILED' }
    } finally {
        if (Test-Path -LiteralPath $selfTestRoot) { Remove-Item -LiteralPath $selfTestRoot -Force -Recurse }
    }
    Write-Output (ConvertTo-Json -Compress ([ordered]@{ event='normal_ui_review_release_self_test_passed'; exactCheckpoint=$true; mismatchRejected=$true; outsideRejected=$true; exclusiveWriteOneWinner=$true }))
    return
}

$root = Resolve-ReviewRuntime $RuntimeRoot
$checkpointFile = [IO.Path]::GetFullPath($CheckpointPath)
$expectedCheckpoint = Join-Path $root 'image-review-checkpoint.json'
if (-not $checkpointFile.Equals($expectedCheckpoint, [StringComparison]::OrdinalIgnoreCase) -or -not (Test-Path -LiteralPath $checkpointFile -PathType Leaf)) { throw 'NORMAL_UI_REVIEW_CHECKPOINT_PATH_INVALID' }
foreach ($value in @($JobId,$ManifestId,$ManifestFingerprint,$RequestDigest)) { Assert-NonEmptyString $value 'NORMAL_UI_REVIEW_ARGUMENT_INVALID' }
if ($RequestDigest -notmatch '^sha256:[0-9a-f]{64}$') { throw 'NORMAL_UI_REVIEW_REQUEST_DIGEST_INVALID' }

$checkpoint = Read-ObjectJson $checkpointFile 'NORMAL_UI_REVIEW_CHECKPOINT_INVALID'
$expected = @{ jobId=$JobId; manifestId=$ManifestId; manifestFingerprint=$ManifestFingerprint; requestDigest=$RequestDigest }
if (-not (Test-CheckpointExact $checkpoint $expected)) { throw 'NORMAL_UI_REVIEW_CHECKPOINT_MISMATCH' }

$proxyConfig = Read-ObjectJson (Join-Path $root 'proxy-config.json') 'NORMAL_UI_REVIEW_PROXY_CONFIG_INVALID'
$scope = Read-ObjectJson (Join-Path $root 'ui-scope-registration.json') 'NORMAL_UI_REVIEW_SCOPE_INVALID'
$binding = Read-ObjectJson (Join-Path $root 'scope-binding.json') 'NORMAL_UI_REVIEW_SOURCE_BINDING_INVALID'
$databasePath = [IO.Path]::GetFullPath([string]$proxyConfig.databasePath)
if ($proxyConfig.runtimeRoot -ne $root -or -not $databasePath.Equals((Join-Path $root 'data\image-client.db'), [StringComparison]::OrdinalIgnoreCase)) { throw 'NORMAL_UI_REVIEW_DATABASE_PATH_INVALID' }
$scopeFields = @('projectId','novelWorkId','novelChapterId','sourceRevisionId','productionJobId','sourceAnalysisRunId')
foreach ($field in $scopeFields) { Assert-NonEmptyString $scope.$field 'NORMAL_UI_REVIEW_SCOPE_INVALID' }
if ($scope.productionJobId -ne $JobId -or $binding.jobId -ne $scope.productionJobId -or $binding.sourceAnalysisRunId -ne $scope.sourceAnalysisRunId) { throw 'NORMAL_UI_REVIEW_BINDING_MISMATCH' }

$gateScopePath = Join-Path $root ('review-gate-' + [Guid]::NewGuid().ToString('N') + '.json')
try {
    Write-AtomicJson $gateScopePath ([ordered]@{ projectId=$scope.projectId; novelWorkId=$scope.novelWorkId; novelChapterId=$scope.novelChapterId; sourceRevisionId=$scope.sourceRevisionId; productionJobId=$scope.productionJobId; sourceAnalysisRunId=$scope.sourceAnalysisRunId })
    $gateOutput = @(& $PythonPath $dbGate --database $databasePath --kind image --scope $gateScopePath 2>&1)
    if ($LASTEXITCODE -ne 0) { throw 'NORMAL_UI_REVIEW_DB_GATE_UNAVAILABLE' }
    try { $gate = ($gateOutput -join "`n") | ConvertFrom-Json } catch { throw 'NORMAL_UI_REVIEW_DB_GATE_INVALID' }
    if (-not $gate.ok -or $gate.result.jobId -ne $JobId -or $gate.result.manifestId -ne $ManifestId -or $gate.result.manifestFingerprint -ne $ManifestFingerprint) { throw 'NORMAL_UI_REVIEW_DB_GATE_MISMATCH' }
} finally {
    if (Test-Path -LiteralPath $gateScopePath -PathType Leaf) { Remove-Item -LiteralPath $gateScopePath -Force }
}

$releasePath = Join-Path $root 'image-review-release.json'
Write-ExclusiveJson $releasePath ([ordered]@{ schemaVersion='normal-ui-image-release.v1'; jobId=$JobId; manifestId=$ManifestId; manifestFingerprint=$ManifestFingerprint; requestDigest=$RequestDigest })
Write-Output (ConvertTo-Json -Compress ([ordered]@{ event='normal_ui_image_review_released'; runtimeRoot=$root; jobId=$JobId; manifestId=$ManifestId; manifestFingerprint=$ManifestFingerprint; requestDigest=$RequestDigest }))
