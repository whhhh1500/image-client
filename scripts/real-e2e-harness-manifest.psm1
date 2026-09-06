Set-StrictMode -Version Latest

function Get-RealE2eHarnessInputs {
    param([Parameter(Mandatory)][string]$ProjectRoot)

    $root = [System.IO.Path]::GetFullPath($ProjectRoot)
    $relativeRoots = @(
        'src-tauri/src',
        'src-tauri/migrations',
        'src-tauri/capabilities',
        'src-tauri/icons',
        'src-tauri/resources',
        'src-tauri/gen',
        'src/shared',
        'dist',
        'docs/小说漫画/验收短篇-雨巷来信.md',
        'src-tauri/Cargo.toml',
        'src-tauri/Cargo.lock',
        'src-tauri/build.rs',
        'src-tauri/tauri.conf.json',
        '.cargo/config.toml',
        'rust-toolchain',
        'rust-toolchain.toml'
    )
    $files = foreach ($relative in $relativeRoots) {
        $path = Join-Path $root $relative
        if (-not (Test-Path -LiteralPath $path)) {
            continue
        }
        if ((Get-Item -LiteralPath $path).PSIsContainer) {
            Get-ChildItem -LiteralPath $path -File -Recurse -Force
        }
        else {
            Get-Item -LiteralPath $path -Force
        }
    }
    @($files |
        Sort-Object -Property FullName -Unique |
        ForEach-Object {
            [ordered]@{
                path = [System.IO.Path]::GetRelativePath($root, $_.FullName).Replace([System.IO.Path]::DirectorySeparatorChar, '/')
                bytes = [int64]$_.Length
                sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
            }
        })
}

function Get-RealE2eHarnessInputsDigest {
    param([Parameter(Mandatory)][object[]]$Inputs)

    $json = ($Inputs | ConvertTo-Json -Depth 5 -Compress)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
        ([System.BitConverter]::ToString($sha.ComputeHash($bytes))).Replace('-', '')
    }
    finally {
        $sha.Dispose()
    }
}

function Get-RealE2eHarnessBuildOptions {
    param([Parameter(Mandatory)][string]$ProjectRoot)

    foreach ($name in @('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'TAURI_CONFIG')) {
        if (-not [string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($name, 'Process'))) {
            throw "E2E_HARNESS_UNEXPECTED_BUILD_ENV_$name"
        }
    }
    $cargoVersion = (& cargo --version).Trim()
    $rustcVersion = (& rustc --version).Trim()
    [ordered]@{
        cargo = 'build'
        cargoVersion = $cargoVersion
        rustcVersion = $rustcVersion
        manifestPath = 'src-tauri/Cargo.toml'
        feature = 'real-e2e-harness'
        targetDir = 'real-e2e-harness-target'
        binary = 'real-e2e-harness-bin/image-client-real-e2e-harness.exe'
        environment = [ordered]@{
            CARGO_BUILD_JOBS = '2'
        }
    }
}

function Get-RealE2eHarnessManifest {
    param([Parameter(Mandatory)][string]$ProjectRoot, [Parameter(Mandatory)][string]$BinaryPath)

    $inputs = Get-RealE2eHarnessInputs -ProjectRoot $ProjectRoot
    if ($inputs.Count -eq 0) {
        throw 'E2E_BUILD_INPUTS_EMPTY'
    }
    if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) {
        throw 'E2E_HARNESS_BINARY_MISSING'
    }
    [ordered]@{
        formatVersion = 1
        feature = 'real-e2e-harness'
        buildOptions = Get-RealE2eHarnessBuildOptions -ProjectRoot $ProjectRoot
        inputsDigest = Get-RealE2eHarnessInputsDigest -Inputs $inputs
        inputs = $inputs
        binary = [ordered]@{
            path = [System.IO.Path]::GetRelativePath($ProjectRoot, $BinaryPath).Replace([System.IO.Path]::DirectorySeparatorChar, '/')
            bytes = [int64](Get-Item -LiteralPath $BinaryPath).Length
            sha256 = (Get-FileHash -LiteralPath $BinaryPath -Algorithm SHA256).Hash
        }
    }
}

function Assert-RealE2eHarnessManifest {
    param([Parameter(Mandatory)][string]$ProjectRoot, [Parameter(Mandatory)][string]$ManifestPath)

    if (-not (Test-Path -LiteralPath $ManifestPath -PathType Leaf)) {
        throw 'E2E_HARNESS_BUILD_MANIFEST_MISSING'
    }
    $manifest = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
    if ($manifest.formatVersion -ne 1 -or $manifest.feature -ne 'real-e2e-harness') {
        throw 'E2E_HARNESS_BUILD_MANIFEST_INVALID'
    }
    $currentOptions = Get-RealE2eHarnessBuildOptions -ProjectRoot $ProjectRoot | ConvertTo-Json -Depth 5 -Compress
    $recordedOptions = $manifest.buildOptions | ConvertTo-Json -Depth 5 -Compress
    if ($currentOptions -ne $recordedOptions) {
        throw 'E2E_HARNESS_BUILD_OPTIONS_CHANGED'
    }
    $inputs = Get-RealE2eHarnessInputs -ProjectRoot $ProjectRoot
    $digest = Get-RealE2eHarnessInputsDigest -Inputs $inputs
    if ($digest -ne $manifest.inputsDigest -or $inputs.Count -ne @($manifest.inputs).Count) {
        throw 'E2E_HARNESS_BUILD_INPUTS_CHANGED'
    }
    for ($index = 0; $index -lt $inputs.Count; $index++) {
        $expected = $manifest.inputs[$index]
        $actual = $inputs[$index]
        if ($expected.path -ne $actual.path -or $expected.bytes -ne $actual.bytes -or $expected.sha256 -ne $actual.sha256) {
            throw 'E2E_HARNESS_BUILD_INPUTS_CHANGED'
        }
    }
    $expectedBinaryRelative = 'real-e2e-harness-bin/image-client-real-e2e-harness.exe'
    if ($manifest.binary.path -ne $expectedBinaryRelative) {
        throw 'E2E_HARNESS_BUILD_MANIFEST_INVALID'
    }
    $binaryPath = Join-Path $ProjectRoot $expectedBinaryRelative.Replace('/', [System.IO.Path]::DirectorySeparatorChar)
    if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
        throw 'E2E_HARNESS_BINARY_MISSING'
    }
    $binary = Get-Item -LiteralPath $binaryPath
    if ($binary.Length -ne [int64]$manifest.binary.bytes -or (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash -ne $manifest.binary.sha256) {
        throw 'E2E_HARNESS_BINARY_STALE'
    }
    [pscustomobject]@{
        Manifest = $manifest
        BinaryPath = $binaryPath
        BinaryHash = $manifest.binary.sha256
    }
}

Export-ModuleMember -Function Get-RealE2eHarnessInputs, Get-RealE2eHarnessInputsDigest, Get-RealE2eHarnessBuildOptions, Get-RealE2eHarnessManifest, Assert-RealE2eHarnessManifest
