[CmdletBinding()]
param(
    [Parameter(ParameterSetName='Launch')]
    [switch]$Run,
    [Parameter(ParameterSetName='Launch')]
    [switch]$ConfirmOneEach,
    [Parameter(ParameterSetName='Launch', Mandatory)]
    [string]$NormalExe,
    [Parameter(ParameterSetName='Launch', Mandatory)]
    [string]$ExpectedNormalSha256,
    [Parameter(ParameterSetName='Launch', Mandatory)]
    [string]$RealEnvPath,
    [Parameter(ParameterSetName='Stop', Mandatory)]
    [switch]$Stop,
    [Parameter(ParameterSetName='Stop', Mandatory)]
    [string]$RuntimeRoot,
    [Parameter(ParameterSetName='SelfTest', Mandatory)]
    [switch]$SelfTest,
    [Parameter(ParameterSetName='Launch')]
    [ValidateRange(60, 1800)]
    [int]$TimeoutSeconds = 900
)

$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$proxyScript = Join-Path $PSScriptRoot 'normal_ui_proxy.mjs'
$dParent = Join-Path $projectRoot '.test-tmp\normal-ui-acceptance-runtime'

function Assert-NoExistingReparseAncestor([Parameter(Mandatory)][string]$Path) {
    $fullPath = [IO.Path]::GetFullPath($Path)
    $root = [IO.Path]::GetPathRoot($fullPath)
    if ([string]::IsNullOrWhiteSpace($root)) { throw 'NORMAL_UI_PATH_ROOT_INVALID' }
    $cursor = $root
    foreach ($part in ($fullPath.Substring($root.Length) -split '[\\/]' | Where-Object { $_ })) {
        $cursor = Join-Path $cursor $part
        if (-not ([IO.File]::Exists($cursor) -or [IO.Directory]::Exists($cursor))) { break }
        if (([IO.File]::GetAttributes($cursor) -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw 'NORMAL_UI_REPARSE_ANCESTOR'
        }
    }
}

function Resolve-DTestChild([Parameter(Mandatory)][string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    $prefix = [IO.Path]::GetFullPath((Join-Path $projectRoot '.test-tmp')).TrimEnd('\','/') + [IO.Path]::DirectorySeparatorChar
    if (-not $full.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'NORMAL_UI_OUTSIDE_TEST_TMP' }
    if (-not ([IO.Path]::GetPathRoot($full).TrimEnd('\','/').Equals('D:', [StringComparison]::OrdinalIgnoreCase))) { throw 'NORMAL_UI_DRIVE_MUST_BE_D' }
    Assert-NoExistingReparseAncestor $full
    return $full
}

function Get-FileSha256([Parameter(Mandatory)][string]$Path) {
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToUpperInvariant()
}

function Get-FreeLoopbackPort {
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    try {
        $listener.Start()
        return ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    }
    finally { $listener.Stop() }
}

function Read-SimpleDotEnv([Parameter(Mandatory)][string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw 'NORMAL_UI_REAL_ENV_MISSING' }
    $values = @{}
    foreach ($raw in [IO.File]::ReadAllLines($Path)) {
        $line = $raw.Trim()
        if ($line.Length -eq 0 -or $line.StartsWith('#')) { continue }
        if ($line -match '^(?i:export)\s' -or $line.Contains('${') -or $line.Contains('$(') -or $line.Contains('`')) { throw 'NORMAL_UI_REAL_ENV_COMPLEX_SYNTAX' }
        $match = [regex]::Match($line, '^([A-Za-z_][A-Za-z0-9_.-]*)\s*=\s*(.*)$')
        if (-not $match.Success) { throw 'NORMAL_UI_REAL_ENV_SYNTAX' }
        $key = $match.Groups[1].Value.ToUpperInvariant().Replace('-', '_')
        $value = $match.Groups[2].Value
        if ($value.StartsWith("'") -or $value.StartsWith('"') -or $value.EndsWith("'") -or $value.EndsWith('"')) { throw 'NORMAL_UI_REAL_ENV_COMPLEX_SYNTAX' }
        if ($values.ContainsKey($key)) { throw 'NORMAL_UI_REAL_ENV_DUPLICATE_KEY' }
        $values[$key] = $value
    }
    foreach ($required in @('LLM_API_URL','LLM_API_KEY','IMAGE_API_URL','IMAGE_API_KEY')) {
        if (-not $values.ContainsKey($required) -or [string]::IsNullOrWhiteSpace($values[$required])) { throw 'NORMAL_UI_REAL_ENV_REQUIRED' }
    }
    return $values
}

function New-LoopbackToken {
    $bytes = [byte[]]::new(32)
    [Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
    return [Convert]::ToBase64String($bytes).TrimEnd('=').Replace('+','-').Replace('/','_')
}

function New-CleanEnvironment([Parameter(Mandatory)][string]$RuntimeRoot) {
    $out = [ordered]@{}
    foreach ($name in @('SystemRoot','WINDIR','SystemDrive','ComSpec','PATH')) {
        $value = [Environment]::GetEnvironmentVariable($name, 'Process')
        if ([string]::IsNullOrWhiteSpace($value)) { throw 'NORMAL_UI_REQUIRED_ENV_MISSING' }
        $out[$name] = $value
    }
    $dataRoot = Join-Path $RuntimeRoot 'data'
    $webViewRoot = Join-Path $RuntimeRoot 'webview2'
    $localAppData = Join-Path $RuntimeRoot 'localappdata'
    $appData = Join-Path $RuntimeRoot 'appdata'
    $userProfile = Join-Path $RuntimeRoot 'userprofile'
    New-Item -ItemType Directory -Force -Path $dataRoot,$webViewRoot,$localAppData,$appData,$userProfile | Out-Null
    $out['TEMP'] = $RuntimeRoot
    $out['TMP'] = $RuntimeRoot
    $out['IMAGE_CLIENT_DATA_DIR'] = $dataRoot
    # WebView2 and Windows profile fallbacks must be isolated as well; none of
    # these values point at the user's C: profile or the repository.
    $out['WEBVIEW2_USER_DATA_FOLDER'] = $webViewRoot
    $out['LOCALAPPDATA'] = $localAppData
    $out['APPDATA'] = $appData
    $out['USERPROFILE'] = $userProfile
    $out['HOMEDRIVE'] = 'D:'
    $out['HOMEPATH'] = $userProfile.Substring(2)
    return $out
}

function Start-OwnedProcess([Parameter(Mandatory)][string]$FileName, [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$Arguments, [Parameter(Mandatory)][System.Collections.IDictionary]$Environment, [Parameter(Mandatory)][string]$WorkingDirectory, [switch]$Visible) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $FileName
    $start.WorkingDirectory = $WorkingDirectory
    $start.UseShellExecute = $false
    $start.CreateNoWindow = -not $Visible
    $start.WindowStyle = if ($Visible) { [Diagnostics.ProcessWindowStyle]::Normal } else { [Diagnostics.ProcessWindowStyle]::Hidden }
    $start.Environment.Clear()
    foreach ($pair in $Environment.GetEnumerator()) { $start.Environment[$pair.Key] = $pair.Value }
    foreach ($argument in $Arguments) { [void]$start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start)
    if ($null -eq $process) { throw 'NORMAL_UI_PROCESS_START_FAILED' }
    return $process
}

function Stop-OwnedProcess([Parameter()][Diagnostics.Process]$Process) {
    if ($null -eq $Process) { return }
    try {
        if (-not $Process.HasExited) {
            # The supervisor only ever supplies roots it started itself.
            $Process.Kill($true)
            [void]$Process.WaitForExit(15000)
        }
    } catch { }
}

function Test-ImmutableStatusValue([Parameter(Mandatory)][string]$Key, [object]$Previous, [object]$Current) {
    if ($Key -notin @('proxyStartedAtUtc','normalStartedAtUtc')) { return [string]$Previous -eq [string]$Current }
    try {
        $previousUtc = if ($Previous -is [DateTime]) {
            $Previous.ToUniversalTime()
        } elseif ($Previous -is [DateTimeOffset]) {
            $Previous.UtcDateTime
        } else {
            [DateTimeOffset]::Parse([string]$Previous, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind).UtcDateTime
        }
        $currentUtc = if ($Current -is [DateTime]) {
            $Current.ToUniversalTime()
        } elseif ($Current -is [DateTimeOffset]) {
            $Current.UtcDateTime
        } else {
            [DateTimeOffset]::Parse([string]$Current, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind).UtcDateTime
        }
        return $previousUtc.Ticks -eq $currentUtc.Ticks
    } catch { return $false }
}

function Write-SupervisorStatus([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][System.Collections.IDictionary]$Value) {
    $immutableKeys = @(
        'schemaVersion','runtimeRoot','proxyPid','proxyStartedAtUtc','proxyScriptPath','proxyConfigPath',
        'normalPid','normalExecutablePath','normalStartedAtUtc','normalSha256','copiedNormalSha256','cdpPort','proxyPort'
    )
    if (Test-Path -LiteralPath $Path -PathType Leaf) {
        try {
            $previous = Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json
            foreach ($key in $immutableKeys) {
                $previousProperty = $previous.PSObject.Properties[$key]
                if ($null -eq $previousProperty -or $null -eq $previousProperty.Value) { continue }
                if (-not $Value.Contains($key) -or $null -eq $Value[$key]) {
                    $Value[$key] = $previousProperty.Value
                    continue
                }
                if (-not (Test-ImmutableStatusValue -Key $key -Previous $previousProperty.Value -Current $Value[$key])) {
                    throw "NORMAL_UI_STATUS_IMMUTABLE_IDENTITY_MUTATION:$key"
                }
            }
        } catch {
            if ($_.Exception.Message -like 'NORMAL_UI_STATUS_IMMUTABLE_IDENTITY_MUTATION:*') { throw }
            throw 'NORMAL_UI_STATUS_PREVIOUS_INVALID'
        }
    }
    [IO.File]::WriteAllText($Path, (ConvertTo-Json -Compress $Value))
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

function Get-SafeLocalCode([object]$Value) {
    $match = [regex]::Match([string]$Value, '\bNORMAL_UI_[A-Z0-9_]+\b')
    if ($match.Success) { return $match.Value }
    return 'NORMAL_UI_SUPERVISOR_LOCAL_FAILURE'
}

function Get-ForwardCounts([Parameter(Mandatory)][string]$AuditPath) {
    $counts = [ordered]@{ source=0; adaptation=0; image=0 }
    if (-not (Test-Path -LiteralPath $AuditPath -PathType Leaf)) { return $counts }
    foreach ($line in [IO.File]::ReadAllLines($AuditPath)) {
        try {
            $event = $line | ConvertFrom-Json
            if ($event.event -eq 'generation_post_reserved' -and $counts.Contains($event.kind)) { $counts[$event.kind] += 1 }
        } catch { return [ordered]@{ source=-1; adaptation=-1; image=-1 } }
    }
    return $counts
}

function Test-ExactStartedAt([Parameter(Mandatory)][Diagnostics.Process]$Process, [Parameter(Mandatory)][object]$ExpectedUtc) {
    try {
        $expected = if ($ExpectedUtc -is [DateTime]) {
            $ExpectedUtc.ToUniversalTime()
        } elseif ($ExpectedUtc -is [DateTimeOffset]) {
            $ExpectedUtc.UtcDateTime
        } else {
            [DateTimeOffset]::Parse(
                [string]$ExpectedUtc,
                [Globalization.CultureInfo]::InvariantCulture,
                [Globalization.DateTimeStyles]::RoundtripKind
            ).UtcDateTime
        }
        return [Math]::Abs(($Process.StartTime.ToUniversalTime() - $expected).TotalSeconds) -le 5
    } catch { return $false }
}

function Get-VerifiedNormalRoot([Parameter(Mandatory)][psobject]$Status) {
    if ($null -eq $Status.normalPid -or [int]$Status.normalPid -le 0) { return [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_NORMAL_PID_MISSING' } }
    try {
        $process = [Diagnostics.Process]::GetProcessById([int]$Status.normalPid)
        $cim = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId=$($Status.normalPid)" -ErrorAction Stop
    } catch { return [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_NORMAL_PID_MISSING' } }
    $expectedPath = if ([string]::IsNullOrWhiteSpace([string]$Status.normalExecutablePath)) { Join-Path $Status.runtimeRoot 'bin\image-client.exe' } else { [string]$Status.normalExecutablePath }
    $pathMatches = -not [string]::IsNullOrWhiteSpace([string]$cim.ExecutablePath) -and [IO.Path]::GetFullPath($process.Path).Equals([IO.Path]::GetFullPath($expectedPath), [StringComparison]::OrdinalIgnoreCase) -and [IO.Path]::GetFullPath($cim.ExecutablePath).Equals([IO.Path]::GetFullPath($expectedPath), [StringComparison]::OrdinalIgnoreCase)
    $commandMatches = -not [string]::IsNullOrWhiteSpace([string]$cim.CommandLine) -and $cim.CommandLine.IndexOf($expectedPath, [StringComparison]::OrdinalIgnoreCase) -ge 0
    # Only an older refused-stop status lacks the start timestamp. Its exact
    # unique runtime/bin path, copied hash, and independently proven proxy are
    # still required for this one recovery cleanup.
    $startedMatches = if ([string]::IsNullOrWhiteSpace([string]$Status.normalStartedAtUtc)) { $process.StartTime.ToUniversalTime() -ge (Get-Item -LiteralPath $Status.runtimeRoot).CreationTimeUtc } else { Test-ExactStartedAt -Process $process -ExpectedUtc $Status.normalStartedAtUtc }
    if (-not $pathMatches -or -not $commandMatches -or -not $startedMatches) { return [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_NORMAL_IDENTITY_MISMATCH' } }
    $expectedHash = if ([string]::IsNullOrWhiteSpace([string]$Status.copiedNormalSha256)) { [string]$Status.normalSha256 } else { [string]$Status.copiedNormalSha256 }
    if ((Get-FileSha256 $expectedPath) -ne $expectedHash) { return [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_NORMAL_HASH_MISMATCH' } }
    return [pscustomobject]@{ Process=$process; Code='NORMAL_UI_STOP_NORMAL_VERIFIED' }
}

function Get-VerifiedProxyRoot([Parameter(Mandatory)][psobject]$Status) {
    if ($null -eq $Status.proxyPid -or [int]$Status.proxyPid -le 0) { return [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_PROXY_PID_MISSING' } }
    try {
        $process = [Diagnostics.Process]::GetProcessById([int]$Status.proxyPid)
        $cim = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId=$($Status.proxyPid)" -ErrorAction Stop
    } catch { return [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_PROXY_PID_MISSING' } }
    try {
        $ready = Get-Content -Raw -LiteralPath (Join-Path $Status.runtimeRoot 'proxy-ready.json') | ConvertFrom-Json
        $backend = Get-Content -Raw -LiteralPath (Join-Path $Status.runtimeRoot 'data\backend-config.json') | ConvertFrom-Json
        $listeners = @(Get-NetTCPConnection -LocalPort ([int]$ready.port) -State Listen -ErrorAction Stop)
    } catch { return [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_PROXY_IDENTITY_UNAVAILABLE' } }
    $pathIsNode = [IO.Path]::GetFileName($process.Path) -in @('node.exe','node') -and [IO.Path]::GetFileName([string]$cim.ExecutablePath) -in @('node.exe','node')
    $commandMatches = -not [string]::IsNullOrWhiteSpace([string]$cim.CommandLine) -and
        $cim.CommandLine.IndexOf([string]$Status.proxyScriptPath, [StringComparison]::OrdinalIgnoreCase) -ge 0 -and
        $cim.CommandLine.IndexOf([string]$Status.proxyConfigPath, [StringComparison]::OrdinalIgnoreCase) -ge 0 -and
        $cim.CommandLine.IndexOf([string]$Status.runtimeRoot, [StringComparison]::OrdinalIgnoreCase) -ge 0
    # New status records include exact start time. A one-time recovery from an
    # older refused-stop status may lack it; the random loopback proof and
    # exact listening PID still prevent a reused arbitrary node process.
    $startedMatches = [string]::IsNullOrWhiteSpace([string]$Status.proxyStartedAtUtc) -or (Test-ExactStartedAt -Process $process -ExpectedUtc $Status.proxyStartedAtUtc)
    $listenerMatches = $ready.schemaVersion -eq 'normal-ui-acceptance-proxy.v1' -and [int]$ready.pid -eq [int]$Status.proxyPid -and $listeners.Count -eq 1 -and [int]$listeners[0].OwningProcess -eq [int]$Status.proxyPid
    if (-not $pathIsNode -or -not $commandMatches -or -not $startedMatches -or -not $listenerMatches -or [string]::IsNullOrWhiteSpace([string]$backend.image_api_key)) {
        return [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_PROXY_IDENTITY_MISMATCH' }
    }
    try {
        $handler = [Net.Http.HttpClientHandler]::new(); $handler.UseProxy = $false
        $client = [Net.Http.HttpClient]::new($handler); $client.Timeout = [TimeSpan]::FromSeconds(3)
        $request = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Post, "http://127.0.0.1:$($ready.port)/__owned_stop_probe")
        $request.Headers.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', [string]$backend.image_api_key)
        $request.Content = [Net.Http.StringContent]::new('{}', [Text.Encoding]::UTF8, 'application/json')
        $response = $client.Send($request); $status = [int]$response.StatusCode; $response.Dispose(); $request.Dispose(); $client.Dispose(); $handler.Dispose()
        if ($status -ne 404) { return [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_PROXY_PROOF_MISMATCH' } }
    } catch { return [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_PROXY_PROOF_UNAVAILABLE' } }
    return [pscustomobject]@{ Process=$process; Code='NORMAL_UI_STOP_PROXY_VERIFIED' }
}

function Test-LoopbackPortReleased([Parameter(Mandatory)][int]$Port) {
    if ($Port -le 0) { return $false }
    return @(Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue).Count -eq 0
}

if ($SelfTest) {
    $current = [Diagnostics.Process]::GetCurrentProcess()
    $exactJson = ConvertTo-Json -Compress ([ordered]@{ startedAtUtc = $current.StartTime.ToUniversalTime().ToString('O') })
    $roundTripped = $exactJson | ConvertFrom-Json
    if (-not (Test-ExactStartedAt -Process $current -ExpectedUtc $roundTripped.startedAtUtc)) {
        throw 'NORMAL_UI_STOP_TIMESTAMP_ROUNDTRIP_FAILED'
    }
    $wrongPlusEight = [DateTimeOffset]::new($current.StartTime.ToUniversalTime().AddHours(8), [TimeSpan]::Zero).ToString('O')
    if (Test-ExactStartedAt -Process $current -ExpectedUtc $wrongPlusEight) {
        throw 'NORMAL_UI_STOP_TIMESTAMP_OFFSET_FALSE_POSITIVE'
    }
    if ((Get-SafeLocalCode 'error NORMAL_UI_PROXY_READY_TIMEOUT') -ne 'NORMAL_UI_PROXY_READY_TIMEOUT' -or (Get-SafeLocalCode 'unstructured local failure') -ne 'NORMAL_UI_SUPERVISOR_LOCAL_FAILURE') {
        throw 'NORMAL_UI_SAFE_LOCAL_CODE_FAILED'
    }
    $selfTestRoot = Join-Path $dParent ('selftest-status-' + [Guid]::NewGuid().ToString('N'))
    try {
        New-Item -ItemType Directory -Path $selfTestRoot | Out-Null
        $selfTestStatus = Join-Path $selfTestRoot 'supervisor-status.json'
        Write-SupervisorStatus $selfTestStatus ([ordered]@{ schemaVersion='normal-ui-acceptance-supervisor.v1'; status='running'; runtimeRoot=$selfTestRoot; proxyPid=101; proxyStartedAtUtc='2026-09-05T00:00:00.0000000+00:00'; proxyScriptPath='D:\safe\proxy.mjs'; proxyConfigPath='D:\safe\proxy.json'; normalPid=$null; normalExecutablePath='D:\safe\image-client.exe'; normalStartedAtUtc=$null; normalSha256='A'; copiedNormalSha256='A'; cdpPort=12345; proxyPort=12346 })
        # ConvertFrom-Json turns ISO strings into DateTime objects by default.
        # A different ISO representation of the same instant must still be
        # accepted for both immutable process-start timestamps.
        Write-SupervisorStatus $selfTestStatus ([ordered]@{ schemaVersion='normal-ui-acceptance-supervisor.v1'; status='running'; runtimeRoot=$selfTestRoot; proxyPid=101; proxyStartedAtUtc='2026-09-05T08:00:00.0000000+08:00'; normalPid=202; normalStartedAtUtc='2026-09-05T08:05:00.0000000+08:00' })
        $preserved = Get-Content -Raw -LiteralPath $selfTestStatus | ConvertFrom-Json
        $preservedProxyUtc = [DateTimeOffset]::Parse([string]$preserved.proxyStartedAtUtc, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind).UtcDateTime
        $preservedNormalUtc = [DateTimeOffset]::Parse([string]$preserved.normalStartedAtUtc, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind).UtcDateTime
        if ($preservedProxyUtc.Ticks -ne [DateTimeOffset]::Parse('2026-09-05T00:00:00.0000000+00:00').UtcDateTime.Ticks -or $preservedNormalUtc.Ticks -ne [DateTimeOffset]::Parse('2026-09-05T00:05:00.0000000+00:00').UtcDateTime.Ticks -or $preserved.proxyScriptPath -ne 'D:\safe\proxy.mjs' -or $preserved.cdpPort -ne 12345 -or $preserved.proxyPort -ne 12346) {
            throw 'NORMAL_UI_STATUS_IDENTITY_NOT_PRESERVED'
        }
        $proxyTimestampMutationRejected = $false
        try { Write-SupervisorStatus $selfTestStatus ([ordered]@{ schemaVersion='normal-ui-acceptance-supervisor.v1'; status='invalid'; runtimeRoot=$selfTestRoot; proxyPid=101; proxyStartedAtUtc='2026-09-05T08:00:00.0000000+00:00'; normalPid=202; normalStartedAtUtc='2026-09-05T08:05:00.0000000+08:00' }) } catch { $proxyTimestampMutationRejected = $_.Exception.Message -eq 'NORMAL_UI_STATUS_IMMUTABLE_IDENTITY_MUTATION:proxyStartedAtUtc' }
        if (-not $proxyTimestampMutationRejected) { throw 'NORMAL_UI_STATUS_PROXY_TIMESTAMP_MUTATION_NOT_REJECTED' }
        $normalTimestampMutationRejected = $false
        try { Write-SupervisorStatus $selfTestStatus ([ordered]@{ schemaVersion='normal-ui-acceptance-supervisor.v1'; status='invalid'; runtimeRoot=$selfTestRoot; proxyPid=101; proxyStartedAtUtc='2026-09-05T08:00:00.0000000+08:00'; normalPid=202; normalStartedAtUtc='2026-09-05T16:05:00.0000000+08:00' }) } catch { $normalTimestampMutationRejected = $_.Exception.Message -eq 'NORMAL_UI_STATUS_IMMUTABLE_IDENTITY_MUTATION:normalStartedAtUtc' }
        if (-not $normalTimestampMutationRejected) { throw 'NORMAL_UI_STATUS_NORMAL_TIMESTAMP_MUTATION_NOT_REJECTED' }
        $mutationRejected = $false
        try { Write-SupervisorStatus $selfTestStatus ([ordered]@{ schemaVersion='normal-ui-acceptance-supervisor.v1'; status='invalid'; runtimeRoot=$selfTestRoot; proxyPid=999 }) } catch { $mutationRejected = $_.Exception.Message -eq 'NORMAL_UI_STATUS_IMMUTABLE_IDENTITY_MUTATION:proxyPid' }
        if (-not $mutationRejected) { throw 'NORMAL_UI_STATUS_IDENTITY_MUTATION_NOT_REJECTED' }
        $selfTestContext = Join-Path $selfTestRoot 'ui-driver-context.json'
        Write-AtomicJson $selfTestContext ([ordered]@{ runtimeRoot=$selfTestRoot; normalPid=321; cdpPort=12345; proxyPort=12346; normalExecutablePath='D:\safe\image-client.exe'; copiedNormalSha256='A'; stopSupervisorPath='D:\safe\supervise.ps1' })
        $context = Get-Content -Raw -LiteralPath $selfTestContext | ConvertFrom-Json
        if ($context.normalPid -ne 321 -or $context.cdpPort -ne 12345 -or $context.proxyPort -ne 12346 -or $context.stopSupervisorPath -ne 'D:\safe\supervise.ps1' -or @(Get-ChildItem -LiteralPath $selfTestRoot -Filter '*.tmp' -File).Count -ne 0) {
            throw 'NORMAL_UI_DRIVER_CONTEXT_ATOMIC_WRITE_FAILED'
        }
        $selfTestEvidence = Join-Path $selfTestRoot 'supervisor-launch-evidence.json'
        Write-AtomicJson $selfTestEvidence ([ordered]@{ schemaVersion='normal-ui-launch-evidence.v1'; event='normal_ui_launch_failed'; runtimeRoot=$selfTestRoot; phase='normal_status_write'; safeErrorCode=(Get-SafeLocalCode 'NORMAL_UI_STATUS_WRITE_FAILED'); forwards=@{source=0;adaptation=0;image=0} })
        $evidence = Get-Content -Raw -LiteralPath $selfTestEvidence | ConvertFrom-Json
        if ($evidence.event -ne 'normal_ui_launch_failed' -or $evidence.safeErrorCode -ne 'NORMAL_UI_STATUS_WRITE_FAILED' -or $evidence.forwards.source -ne 0 -or $evidence.forwards.adaptation -ne 0 -or $evidence.forwards.image -ne 0) {
            throw 'NORMAL_UI_LAUNCH_EVIDENCE_WRITE_FAILED'
        }
    } finally {
        if (Test-Path -LiteralPath $selfTestRoot) { Remove-Item -LiteralPath $selfTestRoot -Force -Recurse }
    }
    Write-Output (ConvertTo-Json -Compress ([ordered]@{ event='normal_ui_supervisor_self_test_passed'; timestampRoundTrip=$true; shiftedByEightHoursRejected=$true; statusTimestampSameInstantAccepted=$true; proxyTimestampMutationRejected=$true; normalTimestampMutationRejected=$true; statusIdentityPreserved=$true; statusIdentityMutationRejected=$true; driverContextAtomicWrite=$true }))
    return
}

if ($Stop) {
    $stopRoot = Resolve-DTestChild $RuntimeRoot
    $stopStatusPath = Join-Path $stopRoot 'supervisor-status.json'
    if (-not (Test-Path -LiteralPath $stopStatusPath -PathType Leaf)) { throw 'NORMAL_UI_STOP_STATUS_MISSING' }
    $stopStatus = Get-Content -Raw -LiteralPath $stopStatusPath | ConvertFrom-Json
    if ($stopStatus.schemaVersion -ne 'normal-ui-acceptance-supervisor.v1' -or $stopStatus.runtimeRoot -ne $stopRoot) {
        throw 'NORMAL_UI_STOP_STATUS_INVALID'
    }
    $checks = @()
    $normalCheck = Get-VerifiedNormalRoot $stopStatus
    $proxyCheck = Get-VerifiedProxyRoot $stopStatus
    # A root that has already exited is safe to classify as stopped only when
    # its status carries a dedicated recorded port and that port has no
    # listener. This never turns an identity mismatch into permission to kill.
    if ($normalCheck.Code -eq 'NORMAL_UI_STOP_NORMAL_PID_MISSING' -and $null -ne $stopStatus.cdpPort -and (Test-LoopbackPortReleased ([int]$stopStatus.cdpPort))) {
        $normalCheck = [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_NORMAL_ALREADY_STOPPED' }
    }
    if ($proxyCheck.Code -eq 'NORMAL_UI_STOP_PROXY_PID_MISSING') {
        try {
            $recordedProxyPort = [int]((Get-Content -Raw -LiteralPath (Join-Path $stopRoot 'proxy-ready.json') | ConvertFrom-Json).port)
            if (Test-LoopbackPortReleased $recordedProxyPort) {
                $proxyCheck = [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_PROXY_ALREADY_STOPPED' }
            }
        } catch { }
    }
    $previousProxyProof = @($stopStatus.stopVerification) -contains 'NORMAL_UI_STOP_PROXY_VERIFIED'
    $recoveryStatusPath = Join-Path $stopRoot 'supervisor-status-before-recovery.json'
    if (-not $previousProxyProof -and (Test-Path -LiteralPath $recoveryStatusPath -PathType Leaf)) {
        try {
            $recoveryStatus = Get-Content -Raw -LiteralPath $recoveryStatusPath | ConvertFrom-Json
            $previousProxyProof = $recoveryStatus.schemaVersion -eq 'normal-ui-acceptance-supervisor.v1' -and $recoveryStatus.runtimeRoot -eq $stopRoot -and (@($recoveryStatus.stopVerification) -contains 'NORMAL_UI_STOP_PROXY_VERIFIED')
        } catch { $previousProxyProof = $false }
    }
    if ($proxyCheck.Code -eq 'NORMAL_UI_STOP_PROXY_PID_MISSING' -and $previousProxyProof) {
        $proxyCheck = [pscustomobject]@{ Process=$null; Code='NORMAL_UI_STOP_PROXY_ALREADY_STOPPED' }
    }
    $checks += $normalCheck.Code, $proxyCheck.Code
    $stopConfig = Join-Path $stopRoot 'data\backend-config.json'
    $stopCounts = Get-ForwardCounts (Join-Path $stopRoot 'normal-ui-proxy.audit.jsonl')
    $verified = $normalCheck.Code -in @('NORMAL_UI_STOP_NORMAL_VERIFIED','NORMAL_UI_STOP_NORMAL_ALREADY_STOPPED') -and $proxyCheck.Code -in @('NORMAL_UI_STOP_PROXY_VERIFIED','NORMAL_UI_STOP_PROXY_ALREADY_STOPPED')
    if ($verified) { foreach ($ownedProcess in @($normalCheck.Process, $proxyCheck.Process) | Where-Object { $null -ne $_ }) { Stop-OwnedProcess $ownedProcess } }
    $cdpReleased = $true
    if ($null -ne $stopStatus.cdpPort -and [int]$stopStatus.cdpPort -gt 0) {
        $deadline = [DateTime]::UtcNow.AddSeconds(5)
        do {
            $cdpReleased = @(Get-NetTCPConnection -LocalPort ([int]$stopStatus.cdpPort) -State Listen -ErrorAction SilentlyContinue).Count -eq 0
            if (-not $cdpReleased) { Start-Sleep -Milliseconds 100 }
        } while (-not $cdpReleased -and [DateTime]::UtcNow -lt $deadline)
    }
    $cleanup = 'retained-pid-verification-failed'
    if ($verified) {
        if (Test-Path -LiteralPath $stopConfig -PathType Leaf) {
            Remove-Item -LiteralPath $stopConfig -Force
            $cleanup='removed-exact-loopback-config'
        } else {
            $cleanup='already-absent-after-verified-stop'
        }
    }
    $stopped = $verified -and $cdpReleased
    Write-SupervisorStatus $stopStatusPath ([ordered]@{ schemaVersion='normal-ui-acceptance-supervisor.v1'; status=if($stopped){'stopped'}elseif($verified){'stop_cdp_port_retained'}else{'stop_refused_pid_mismatch'}; runtimeRoot=$stopRoot; proxyPid=$stopStatus.proxyPid; proxyStartedAtUtc=$stopStatus.proxyStartedAtUtc; proxyScriptPath=$stopStatus.proxyScriptPath; proxyConfigPath=$stopStatus.proxyConfigPath; normalPid=$stopStatus.normalPid; normalExecutablePath=$stopStatus.normalExecutablePath; normalStartedAtUtc=$stopStatus.normalStartedAtUtc; normalSha256=$stopStatus.normalSha256; copiedNormalSha256=$stopStatus.copiedNormalSha256; cdpPort=$stopStatus.cdpPort; proxyPort=$stopStatus.proxyPort; cdpPortReleased=$cdpReleased; configurationCleanup=$cleanup; forwards=$stopCounts; stopVerification=@($checks) })
    Write-Output (ConvertTo-Json -Compress ([ordered]@{ event=if($stopped){'normal_ui_stopped'}elseif($verified){'normal_ui_stop_cdp_port_retained'}else{'normal_ui_stop_refused'}; runtimeRoot=$stopRoot; forwards=$stopCounts; stopVerification=@($checks); cdpPortReleased=$cdpReleased }))
    exit 0
}

$normal = [IO.Path]::GetFullPath($NormalExe)
if (-not (Test-Path -LiteralPath $normal -PathType Leaf)) { throw 'NORMAL_UI_EXE_MISSING' }
$normalHash = Get-FileSha256 $normal
if (-not $normalHash.Equals($ExpectedNormalSha256, [StringComparison]::OrdinalIgnoreCase)) { throw 'NORMAL_UI_EXE_HASH_MISMATCH' }
if (-not (Test-Path -LiteralPath $proxyScript -PathType Leaf)) { throw 'NORMAL_UI_PROXY_SCRIPT_MISSING' }

if (-not $Run -or -not $ConfirmOneEach) {
    Write-Output (ConvertTo-Json -Compress ([ordered]@{ event='dry_run_verified'; normalSha256=$normalHash; forwards=0 }))
    exit 0
}

$real = Read-SimpleDotEnv ([IO.Path]::GetFullPath($RealEnvPath))
$parent = Resolve-DTestChild $dParent
New-Item -ItemType Directory -Force -Path $parent | Out-Null
Assert-NoExistingReparseAncestor $parent
$runtimeRoot = Resolve-DTestChild (Join-Path $parent ("normal-ui-{0}" -f ([guid]::NewGuid().ToString('N'))))
New-Item -ItemType Directory -Path $runtimeRoot | Out-Null
Assert-NoExistingReparseAncestor $runtimeRoot

$proxyPort = Get-FreeLoopbackPort
$cdpPort = Get-FreeLoopbackPort
$token = New-LoopbackToken
$binRoot = Join-Path $runtimeRoot 'bin'
$copiedExe = Join-Path $binRoot 'image-client.exe'
$dataRoot = Join-Path $runtimeRoot 'data'
$backendConfig = Join-Path $dataRoot 'backend-config.json'
$proxyConfig = Join-Path $runtimeRoot 'proxy-config.json'
$proxyReady = Join-Path $runtimeRoot 'proxy-ready.json'
$scopePath = Join-Path $runtimeRoot 'ui-scope-registration.json'
$checkpointPath = Join-Path $runtimeRoot 'image-review-checkpoint.json'
$releasePath = Join-Path $runtimeRoot 'image-review-release.json'
$auditPath = Join-Path $runtimeRoot 'normal-ui-proxy.audit.jsonl'
$statusPath = Join-Path $runtimeRoot 'supervisor-status.json'
$driverContextPath = Join-Path $runtimeRoot 'ui-driver-context.json'
$launchEvidencePath = Join-Path $runtimeRoot 'supervisor-launch-evidence.json'

$llmModel = if ($real.ContainsKey('LLM_API_MODEL') -and -not [string]::IsNullOrWhiteSpace($real['LLM_API_MODEL'])) { $real['LLM_API_MODEL'] } else { 'gemini-3.7-flash' }
$imageModel = if ($real.ContainsKey('IMAGE_API_MODEL') -and -not [string]::IsNullOrWhiteSpace($real['IMAGE_API_MODEL'])) { $real['IMAGE_API_MODEL'] } else { 'gpt-image-2' }
New-Item -ItemType Directory -Force -Path $binRoot,$dataRoot | Out-Null
Copy-Item -LiteralPath $normal -Destination $copiedExe -Force
$copiedHash = Get-FileSha256 $copiedExe
if ($copiedHash -ne $normalHash) { throw 'NORMAL_UI_COPIED_EXE_HASH_MISMATCH' }
[IO.File]::WriteAllText($backendConfig, (ConvertTo-Json -Compress ([ordered]@{
    image_api_url = "http://127.0.0.1:$proxyPort/v1/images/generations"; image_api_key = $token; image_model = $imageModel
    video_api_url = ''; video_api_key = ''; video_model = 'kling-video-v3'
    llm_api_url = "http://127.0.0.1:$proxyPort/v1"; llm_api_key = $token; llm_model = $llmModel
    output_dir = '$DEFAULT_ASSETS'; source = 'normal-ui-acceptance-loopback'
})))
[IO.File]::WriteAllText($proxyConfig, (ConvertTo-Json -Compress ([ordered]@{
    runtimeRoot=$runtimeRoot; databasePath=(Join-Path $dataRoot 'image-client.db'); auditPath=$auditPath; scopePath=$scopePath
    # Scope registration waits at most 30 seconds in the UI helper.  The proxy
    # waits longer so it cannot race that same bounded read-only registration.
    checkpointPath=$checkpointPath; releasePath=$releasePath; readyPath=$proxyReady; scopeWaitMs=60000; imageReviewWaitMs=180000
})))
$proxy = $null
$app = $null
$configCleanup = 'pending-owner-stop'
$launched = $false
$launchPhase = 'proxy_starting'
$launchSafeErrorCode = $null
try {
    $proxyEnv = New-CleanEnvironment $runtimeRoot
    $proxyEnv['UI_PROXY_TOKEN'] = $token
    # The private loopback token is supplied to the proxy through its child
    # environment. It is not placed in argv and the backend config contains no
    # real provider URL or credential (only the loopback token the app needs).
    $proxy = Start-OwnedProcess -FileName 'node' -Arguments @($proxyScript,'--config',$proxyConfig,'--real-env',([IO.Path]::GetFullPath($RealEnvPath)),'--port',"$proxyPort") -Environment $proxyEnv -WorkingDirectory $runtimeRoot
    $launchPhase = 'proxy_ready_wait'
    $readyDeadline = [DateTime]::UtcNow.AddSeconds(15)
    while (-not (Test-Path -LiteralPath $proxyReady -PathType Leaf) -and [DateTime]::UtcNow -lt $readyDeadline) {
        if ($proxy.HasExited) { throw 'NORMAL_UI_PROXY_EXITED_BEFORE_READY' }
        Start-Sleep -Milliseconds 100
    }
    if (-not (Test-Path -LiteralPath $proxyReady -PathType Leaf)) { throw 'NORMAL_UI_PROXY_READY_TIMEOUT' }
    $ready = Get-Content -Raw -LiteralPath $proxyReady | ConvertFrom-Json
    if ($ready.port -ne $proxyPort) { throw 'NORMAL_UI_PROXY_PORT_MISMATCH' }
    $launchPhase = 'proxy_status_write'
    Write-SupervisorStatus $statusPath ([ordered]@{ schemaVersion='normal-ui-acceptance-supervisor.v1'; status='running'; runtimeRoot=$runtimeRoot; proxyPid=$proxy.Id; proxyStartedAtUtc=$proxy.StartTime.ToUniversalTime().ToString('O'); proxyScriptPath=$proxyScript; proxyConfigPath=$proxyConfig; normalPid=$null; normalExecutablePath=$copiedExe; normalStartedAtUtc=$null; normalSha256=$normalHash; copiedNormalSha256=$copiedHash; cdpPort=$cdpPort; proxyPort=$proxyPort; forwards=@{source=0;adaptation=0;image=0} })

    $launchPhase = 'normal_starting'
    $appEnv = New-CleanEnvironment $runtimeRoot
    $appEnv['WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS'] = "--remote-debugging-address=127.0.0.1 --remote-debugging-port=$cdpPort"
    # This is deliberately visible: a separate UI-only driver performs the
    # normal click/type acceptance flow against this process.
    $app = Start-OwnedProcess -FileName $copiedExe -Arguments @() -Environment $appEnv -WorkingDirectory $binRoot -Visible
    $launchPhase = 'normal_status_write'
    Write-SupervisorStatus $statusPath ([ordered]@{ schemaVersion='normal-ui-acceptance-supervisor.v1'; status='running'; runtimeRoot=$runtimeRoot; proxyPid=$proxy.Id; proxyStartedAtUtc=$proxy.StartTime.ToUniversalTime().ToString('O'); proxyScriptPath=$proxyScript; proxyConfigPath=$proxyConfig; normalPid=$app.Id; normalExecutablePath=$copiedExe; normalStartedAtUtc=$app.StartTime.ToUniversalTime().ToString('O'); normalSha256=$normalHash; copiedNormalSha256=$copiedHash; cdpPort=$cdpPort; proxyPort=$proxyPort; forwards=@{source=0;adaptation=0;image=0}; timeoutSeconds=$TimeoutSeconds })
    $launchPhase = 'driver_context_write'
    Write-AtomicJson $driverContextPath ([ordered]@{
        schemaVersion='normal-ui-acceptance-driver.v1'; runtimeRoot=$runtimeRoot; databasePath=(Join-Path $dataRoot 'image-client.db')
        scopeRegistrationPath=$scopePath; imageCheckpointPath=$checkpointPath; auditPath=$auditPath; cdpPort=$cdpPort; proxyPort=$proxyPort
        normalPid=$app.Id; normalExecutablePath=$copiedExe; normalStartedAtUtc=$app.StartTime.ToUniversalTime().ToString('O')
        normalSha256=$normalHash; copiedNormalSha256=$copiedHash; stopSupervisorPath=$PSCommandPath
    })
    $launched = $true
    Write-Output (ConvertTo-Json -Compress ([ordered]@{ event='normal_ui_started'; runtimeRoot=$runtimeRoot; proxyPid=$proxy.Id; normalPid=$app.Id; cdpPort=$cdpPort; normalSha256=$normalHash; forwards=0; stopCommand="& '$PSCommandPath' -Stop -RuntimeRoot '$runtimeRoot'" }))
} catch {
    $launchSafeErrorCode = Get-SafeLocalCode $_.Exception.Message
    Write-AtomicJson $launchEvidencePath ([ordered]@{
        schemaVersion='normal-ui-launch-evidence.v1'; event='normal_ui_launch_failed'; runtimeRoot=$runtimeRoot
        phase=$launchPhase; safeErrorCode=$launchSafeErrorCode; forwards=@{source=0;adaptation=0;image=0}
    })
    Write-Output (ConvertTo-Json -Compress ([ordered]@{ event='normal_ui_launch_failed'; runtimeRoot=$runtimeRoot; phase=$launchPhase; safeErrorCode=$launchSafeErrorCode; forwards=0 }))
    throw
}
finally {
    if (-not $launched) {
        Stop-OwnedProcess $app
        Stop-OwnedProcess $proxy
        if (Test-Path -LiteralPath $backendConfig -PathType Leaf) {
        Remove-Item -LiteralPath $backendConfig -Force
        $configCleanup = 'removed-exact-loopback-config'
        }
        Write-SupervisorStatus $statusPath ([ordered]@{ schemaVersion='normal-ui-acceptance-supervisor.v1'; status='launch_failed'; runtimeRoot=$runtimeRoot; proxyPid=if($null -eq $proxy){$null}else{$proxy.Id}; normalPid=if($null -eq $app){$null}else{$app.Id}; normalSha256=$normalHash; launchPhase=$launchPhase; safeErrorCode=$launchSafeErrorCode; configurationCleanup=$configCleanup; forwards=@{source=0;adaptation=0;image=0} })
    }
}
