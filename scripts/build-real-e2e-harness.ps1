[CmdletBinding()]
param(
    [switch]$VerifyOnly
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$targetDir = Join-Path $projectRoot 'real-e2e-harness-target'
$binDir = Join-Path $projectRoot 'real-e2e-harness-bin'
$binary = Join-Path $binDir 'image-client-real-e2e-harness.exe'
$manifestPath = Join-Path $binDir 'image-client-real-e2e-harness.build-manifest.json'

Import-Module (Join-Path $PSScriptRoot 'real-e2e-harness-manifest.psm1') -Force

if ($VerifyOnly) {
    $verified = Assert-RealE2eHarnessManifest -ProjectRoot $projectRoot -ManifestPath $manifestPath
    Write-Host "Harness binary verified: $($verified.BinaryPath)"
    Write-Host "Harness SHA-256: $($verified.BinaryHash)"
    exit 0
}

# Snapshot every compiled/embedded input before building.  If any source or
# effective build option changes while Cargo runs, do not copy a binary or
# create a manifest which could falsely describe a newer source tree.
$inputsBefore = Get-RealE2eHarnessInputs -ProjectRoot $projectRoot
$digestBefore = Get-RealE2eHarnessInputsDigest -Inputs $inputsBefore
$optionsBefore = Get-RealE2eHarnessBuildOptions -ProjectRoot $projectRoot | ConvertTo-Json -Depth 5 -Compress

# A dedicated target directory keeps feature artifacts away from normal debug
# and release outputs.  This script builds only; it never starts the harness.
$previousJobs = [Environment]::GetEnvironmentVariable('CARGO_BUILD_JOBS', 'Process')
[Environment]::SetEnvironmentVariable('CARGO_BUILD_JOBS', '2', 'Process')
try {
    & cargo build --manifest-path (Join-Path $projectRoot 'src-tauri\Cargo.toml') --features real-e2e-harness --target-dir $targetDir
    if ($LASTEXITCODE -ne 0) {
        throw "E2E_HARNESS_BUILD_FAILED_$LASTEXITCODE"
    }
}
finally {
    [Environment]::SetEnvironmentVariable('CARGO_BUILD_JOBS', $previousJobs, 'Process')
}
$compiled = Join-Path $targetDir 'debug\image-client.exe'
if (-not (Test-Path -LiteralPath $compiled -PathType Leaf)) {
    throw 'E2E_HARNESS_BUILD_OUTPUT_MISSING'
}
$inputsAfter = Get-RealE2eHarnessInputs -ProjectRoot $projectRoot
$digestAfter = Get-RealE2eHarnessInputsDigest -Inputs $inputsAfter
$optionsAfter = Get-RealE2eHarnessBuildOptions -ProjectRoot $projectRoot | ConvertTo-Json -Depth 5 -Compress
if ($digestBefore -ne $digestAfter -or $optionsBefore -ne $optionsAfter) {
    throw 'E2E_HARNESS_BUILD_INPUTS_CHANGED_DURING_BUILD'
}
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
Copy-Item -LiteralPath $compiled -Destination $binary -Force
$manifest = Get-RealE2eHarnessManifest -ProjectRoot $projectRoot -BinaryPath $binary
$json = $manifest | ConvertTo-Json -Depth 8
[System.IO.File]::WriteAllText($manifestPath, $json + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
$verified = Assert-RealE2eHarnessManifest -ProjectRoot $projectRoot -ManifestPath $manifestPath
Write-Host "Harness binary built: $($verified.BinaryPath)"
Write-Host "Harness SHA-256: $($verified.BinaryHash)"
