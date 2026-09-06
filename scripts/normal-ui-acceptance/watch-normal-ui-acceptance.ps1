[CmdletBinding()]
param(
    [Parameter(ParameterSetName='Watch', Mandatory)]
    [string]$NormalExe,
    [Parameter(ParameterSetName='Watch', Mandatory)]
    [string]$ExpectedNormalSha256,
    [Parameter(ParameterSetName='Watch', Mandatory)]
    [string]$RealEnvPath,
    [Parameter(ParameterSetName='Watch')]
    [string]$SupervisorScript = (Join-Path $PSScriptRoot 'supervise-normal-ui-acceptance.ps1'),
    [Parameter(ParameterSetName='Watch')]
    [ValidateRange(60, 1800)]
    [int]$TimeoutSeconds = 900,
    [Parameter(ParameterSetName='SelfTest', Mandatory)]
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'

function Test-ProcessAlive([object]$ProcessId) {
    if ($null -eq $ProcessId -or [int]$ProcessId -le 0) { return $false }
    try { [void][Diagnostics.Process]::GetProcessById([int]$ProcessId); return $true } catch { return $false }
}

function Test-LoopbackPortReleased([object]$Port) {
    if ($null -eq $Port -or [int]$Port -le 0) { return $false }
    return @(Get-NetTCPConnection -LocalPort ([int]$Port) -State Listen -ErrorAction SilentlyContinue).Count -eq 0
}

function Get-FreeLoopbackPort {
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    try {
        $listener.Start()
        return ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    } finally { $listener.Stop() }
}

function Test-StrictStoppedStatus([Parameter(Mandatory)][psobject]$Status, [Parameter(Mandatory)][psobject]$Context, [Parameter(Mandatory)][string]$RuntimeRoot) {
    try {
        $expectedRoot = [IO.Path]::GetFullPath($RuntimeRoot)
        $statusRoot = [IO.Path]::GetFullPath([string]$Status.runtimeRoot)
        $contextRoot = [IO.Path]::GetFullPath([string]$Context.runtimeRoot)
    } catch { return $false }
    if ($Status.schemaVersion -ne 'normal-ui-acceptance-supervisor.v1' -or $Status.status -ne 'stopped' -or $statusRoot -ne $expectedRoot) { return $false }
    if ($Context.schemaVersion -ne 'normal-ui-acceptance-driver.v1' -or $contextRoot -ne $expectedRoot) { return $false }
    if ($null -eq $Status.normalPid -or $null -eq $Status.proxyPid -or $null -eq $Status.cdpPort -or $null -eq $Status.proxyPort) { return $false }
    if ($null -eq $Context.normalPid -or $null -eq $Context.cdpPort -or $null -eq $Context.proxyPort) { return $false }
    if ([int]$Context.normalPid -ne [int]$Status.normalPid -or [int]$Context.cdpPort -ne [int]$Status.cdpPort -or [int]$Context.proxyPort -ne [int]$Status.proxyPort) { return $false }
    if ($Status.cdpPortReleased -ne $true -or $Status.configurationCleanup -notin @('removed-exact-loopback-config','already-absent-after-verified-stop')) { return $false }
    $checks = @($Status.stopVerification)
    $normalVerified = @('NORMAL_UI_STOP_NORMAL_VERIFIED','NORMAL_UI_STOP_NORMAL_ALREADY_STOPPED') | Where-Object { $checks -contains $_ }
    $proxyVerified = @('NORMAL_UI_STOP_PROXY_VERIFIED','NORMAL_UI_STOP_PROXY_ALREADY_STOPPED') | Where-Object { $checks -contains $_ }
    return $normalVerified.Count -eq 1 -and $proxyVerified.Count -eq 1
}

function Test-ObservedExternalStrictStop([Parameter(Mandatory)][psobject]$Status, [Parameter(Mandatory)][psobject]$Context, [Parameter(Mandatory)][string]$RuntimeRoot) {
    if (-not (Test-StrictStoppedStatus $Status $Context $RuntimeRoot)) { return $false }
    if ((Test-ProcessAlive $Status.normalPid) -or (Test-ProcessAlive $Status.proxyPid)) { return $false }
    if (-not (Test-LoopbackPortReleased $Context.cdpPort) -or -not (Test-LoopbackPortReleased $Context.proxyPort)) { return $false }
    $configPath = Join-Path $RuntimeRoot 'data\backend-config.json'
    if (Test-Path -LiteralPath $configPath -PathType Leaf) { return $false }
    return $true
}

function Get-WatchDecision([Parameter(Mandatory)][psobject]$Status, [bool]$NormalAlive, [bool]$ProxyAlive, [Parameter(Mandatory)][DateTime]$DeadlineUtc) {
    if ($Status.status -eq 'stopped') { return 'stopped' }
    if ($Status.status -ne 'running') { return 'unexpected_status' }
    if (-not $NormalAlive) { return 'normal_exited' }
    if (-not $ProxyAlive) { return 'proxy_exited' }
    if ([DateTime]::UtcNow -ge $DeadlineUtc) { return 'timeout' }
    return 'continue'
}

function Convert-JsonRecords([object[]]$Lines) {
    $records = @()
    foreach ($line in $Lines) {
        try {
            $value = ([string]$line).Trim()
            if ($value.StartsWith('{')) { $records += ($value | ConvertFrom-Json) }
        } catch { }
    }
    return $records
}

function Get-SafeLaunchFailure([object[]]$Records) {
    $failure = @($Records | Where-Object { $_.event -eq 'normal_ui_launch_failed' }) | Select-Object -Last 1
    if ($null -eq $failure) { return $null }
    $code = [string]$failure.safeErrorCode
    if ($code -notmatch '^NORMAL_UI_[A-Z0-9_]+$') { $code = 'NORMAL_UI_SUPERVISOR_LOCAL_FAILURE' }
    return [pscustomobject]@{ RuntimeRoot=[string]$failure.runtimeRoot; Phase=[string]$failure.phase; SafeErrorCode=$code }
}

if ($SelfTest) {
    $deadline = [DateTime]::UtcNow.AddSeconds(60)
    $running = [pscustomobject]@{ status='running' }
    $stopped = [pscustomobject]@{ status='stopped' }
    if ((Get-WatchDecision $running $true $true $deadline) -ne 'continue') { throw 'NORMAL_UI_WATCH_SELFTEST_RUNNING_FAILED' }
    if ((Get-WatchDecision $stopped $false $false $deadline) -ne 'stopped') { throw 'NORMAL_UI_WATCH_SELFTEST_STOPPED_FAILED' }
    if ((Get-WatchDecision $running $false $true $deadline) -ne 'normal_exited') { throw 'NORMAL_UI_WATCH_SELFTEST_NORMAL_EXIT_FAILED' }
    if ((Get-WatchDecision $running $true $false $deadline) -ne 'proxy_exited') { throw 'NORMAL_UI_WATCH_SELFTEST_PROXY_EXIT_FAILED' }
    if ((Get-WatchDecision $running $true $true ([DateTime]::UtcNow.AddSeconds(-1))) -ne 'timeout') { throw 'NORMAL_UI_WATCH_SELFTEST_TIMEOUT_FAILED' }
    $strictContext = [pscustomobject]@{ schemaVersion='normal-ui-acceptance-driver.v1'; runtimeRoot='D:\safe'; normalPid=11; cdpPort=(Get-FreeLoopbackPort); proxyPort=(Get-FreeLoopbackPort) }
    $strictStopped = [pscustomobject]@{ schemaVersion='normal-ui-acceptance-supervisor.v1'; status='stopped'; runtimeRoot='D:\safe'; normalPid=11; proxyPid=12; cdpPort=$strictContext.cdpPort; proxyPort=$strictContext.proxyPort; cdpPortReleased=$true; configurationCleanup='already-absent-after-verified-stop'; stopVerification=@('NORMAL_UI_STOP_NORMAL_ALREADY_STOPPED','NORMAL_UI_STOP_PROXY_ALREADY_STOPPED') }
    if (-not (Test-StrictStoppedStatus $strictStopped $strictContext 'D:\safe')) { throw 'NORMAL_UI_WATCH_SELFTEST_EXTERNAL_STOP_FAILED' }
    $strictStopped.configurationCleanup='retained-pid-verification-failed'
    if (Test-StrictStoppedStatus $strictStopped $strictContext 'D:\safe') { throw 'NORMAL_UI_WATCH_SELFTEST_FORGED_STOP_ACCEPTED' }
    $externalRoot = Join-Path 'D:\cc\image-client\.test-tmp' ("watch-external-stop-selftest-" + [Guid]::NewGuid().ToString('N'))
    try {
        New-Item -ItemType Directory -Path $externalRoot -Force | Out-Null
        $externalProxyPort = Get-FreeLoopbackPort
        [IO.File]::WriteAllText((Join-Path $externalRoot 'proxy-ready.json'), (ConvertTo-Json -Compress ([ordered]@{ port=$externalProxyPort })))
        $externalContext = [pscustomobject]@{ schemaVersion='normal-ui-acceptance-driver.v1'; runtimeRoot=$externalRoot; normalPid=0; cdpPort=(Get-FreeLoopbackPort); proxyPort=(Get-FreeLoopbackPort) }
        $externalStopped = [pscustomobject]@{ schemaVersion='normal-ui-acceptance-supervisor.v1'; status='stopped'; runtimeRoot=$externalRoot; normalPid=0; proxyPid=0; cdpPort=$externalContext.cdpPort; proxyPort=$externalContext.proxyPort; cdpPortReleased=$true; configurationCleanup='already-absent-after-verified-stop'; stopVerification=@('NORMAL_UI_STOP_NORMAL_ALREADY_STOPPED','NORMAL_UI_STOP_PROXY_ALREADY_STOPPED') }
        if (-not (Test-ObservedExternalStrictStop $externalStopped $externalContext $externalRoot)) { throw 'NORMAL_UI_WATCH_SELFTEST_EXTERNAL_STOP_OBSERVATION_FAILED' }
        $forgedContext = [pscustomobject]@{ schemaVersion='normal-ui-acceptance-driver.v1'; runtimeRoot=$externalRoot; normalPid=0; cdpPort=$externalContext.cdpPort; proxyPort=0 }
        if (Test-ObservedExternalStrictStop $externalStopped $forgedContext $externalRoot) { throw 'NORMAL_UI_WATCH_SELFTEST_INCOMPLETE_STOP_ACCEPTED' }
        $forgedContext.proxyPort = Get-FreeLoopbackPort
        $forgedContext.runtimeRoot = 'D:\other'
        if (Test-ObservedExternalStrictStop $externalStopped $forgedContext $externalRoot) { throw 'NORMAL_UI_WATCH_SELFTEST_FORGED_RUNTIME_ACCEPTED' }
    } finally {
        if (Test-Path -LiteralPath $externalRoot) { Remove-Item -LiteralPath $externalRoot -Recurse -Force }
    }
    $parsed = Convert-JsonRecords @('not json','{"event":"normal_ui_started","runtimeRoot":"D:\\safe"}','{"event":"normal_ui_launch_failed","runtimeRoot":"D:\\safe","phase":"normal_status_write","safeErrorCode":"NORMAL_UI_STATUS_WRITE_FAILED"}')
    $failure = Get-SafeLaunchFailure $parsed
    if ($parsed.Count -ne 2 -or $parsed[0].event -ne 'normal_ui_started' -or $null -eq $failure -or $failure.SafeErrorCode -ne 'NORMAL_UI_STATUS_WRITE_FAILED') { throw 'NORMAL_UI_WATCH_SELFTEST_RECORD_PARSE_FAILED' }
    Write-Output (ConvertTo-Json -Compress ([ordered]@{ event='normal_ui_watch_self_test_passed'; running=$true; stopped=$true; processExit=$true; timeout=$true; externalStrictStop=$true; records=$true }))
    return
}

$supervisor = [IO.Path]::GetFullPath($SupervisorScript)
if (-not (Test-Path -LiteralPath $supervisor -PathType Leaf)) { throw 'NORMAL_UI_WATCH_SUPERVISOR_MISSING' }
$runtimeRoot = $null
$watchOutcome = 'launch_failed'
$stopResult = $null
try {
    $launchOutput = @(& pwsh -NoProfile -File $supervisor -Run -ConfirmOneEach -NormalExe $NormalExe -ExpectedNormalSha256 $ExpectedNormalSha256 -RealEnvPath $RealEnvPath -TimeoutSeconds $TimeoutSeconds 2>&1)
    $launchRecords = @(Convert-JsonRecords $launchOutput)
    $launchFailure = Get-SafeLaunchFailure $launchRecords
    if ($LASTEXITCODE -ne 0) {
        if ($null -ne $launchFailure) {
            if (-not [string]::IsNullOrWhiteSpace($launchFailure.RuntimeRoot)) { $runtimeRoot = [IO.Path]::GetFullPath($launchFailure.RuntimeRoot) }
            Write-Output (ConvertTo-Json -Compress ([ordered]@{ event='normal_ui_watch_launch_failed'; runtimeRoot=$runtimeRoot; phase=$launchFailure.Phase; safeErrorCode=$launchFailure.SafeErrorCode; forwards=0 }))
            throw "NORMAL_UI_WATCH_SUPERVISOR_LAUNCH_FAILED:$($launchFailure.SafeErrorCode)"
        }
        throw 'NORMAL_UI_WATCH_SUPERVISOR_LAUNCH_FAILED:NORMAL_UI_SUPERVISOR_LOCAL_FAILURE'
    }
    $launch = @($launchRecords | Where-Object { $_.event -eq 'normal_ui_started' }) | Select-Object -Last 1
    if ($null -eq $launch -or [string]::IsNullOrWhiteSpace([string]$launch.runtimeRoot)) { throw 'NORMAL_UI_WATCH_LAUNCH_RECORD_MISSING' }
    $runtimeRoot = [IO.Path]::GetFullPath([string]$launch.runtimeRoot)
    $contextPath = Join-Path $runtimeRoot 'ui-driver-context.json'
    $statusPath = Join-Path $runtimeRoot 'supervisor-status.json'
    if (-not (Test-Path -LiteralPath $contextPath -PathType Leaf) -or -not (Test-Path -LiteralPath $statusPath -PathType Leaf)) { throw 'NORMAL_UI_WATCH_CONTEXT_MISSING' }
    $context = Get-Content -Raw -LiteralPath $contextPath | ConvertFrom-Json
    $initialStatus = Get-Content -Raw -LiteralPath $statusPath | ConvertFrom-Json
    if ($initialStatus.status -eq 'stopped') {
        if (-not (Test-ObservedExternalStrictStop $initialStatus $context $runtimeRoot)) { throw 'NORMAL_UI_WATCH_STOPPED_STATUS_INVALID' }
        $watchOutcome = 'stopped'
        Write-Output (ConvertTo-Json -Compress ([ordered]@{ event='normal_ui_watch_observed_external_stop'; runtimeRoot=$runtimeRoot; forwards=$initialStatus.forwards }))
    } else {
    if ($context.runtimeRoot -ne $runtimeRoot -or -not (Test-ProcessAlive $context.normalPid)) {
        $stoppedAfterContextRead = Get-Content -Raw -LiteralPath $statusPath | ConvertFrom-Json
        if ($stoppedAfterContextRead.status -eq 'stopped' -and (Test-ObservedExternalStrictStop $stoppedAfterContextRead $context $runtimeRoot)) {
            $watchOutcome = 'stopped'
            Write-Output (ConvertTo-Json -Compress ([ordered]@{ event='normal_ui_watch_observed_external_stop'; runtimeRoot=$runtimeRoot; forwards=$stoppedAfterContextRead.forwards }))
        } else { throw 'NORMAL_UI_WATCH_CONTEXT_IDENTITY_INVALID' }
    }
    if ($watchOutcome -ne 'stopped') {
    Write-Output (ConvertTo-Json -Compress -Depth 4 ([ordered]@{ event='normal_ui_watch_started'; runtimeRoot=$runtimeRoot; normalPid=$context.normalPid; cdpPort=$context.cdpPort; timeoutSeconds=$TimeoutSeconds; driverContext=$context }))
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $status = Get-Content -Raw -LiteralPath $statusPath | ConvertFrom-Json
        $watchOutcome = Get-WatchDecision $status (Test-ProcessAlive $status.normalPid) (Test-ProcessAlive $status.proxyPid) $deadline
        if ($watchOutcome -eq 'stopped' -and -not (Test-ObservedExternalStrictStop $status $context $runtimeRoot)) { throw 'NORMAL_UI_WATCH_STOPPED_STATUS_INVALID' }
        if ($watchOutcome -eq 'continue') { Start-Sleep -Seconds 1 }
    } while ($watchOutcome -eq 'continue')
    if ($watchOutcome -ne 'stopped') { throw "NORMAL_UI_WATCH_$($watchOutcome.ToUpperInvariant())" }
    }
    }
} finally {
    if (-not [string]::IsNullOrWhiteSpace($runtimeRoot)) {
        $stopOutput = @(& pwsh -NoProfile -File $supervisor -Stop -RuntimeRoot $runtimeRoot 2>&1)
        $stopResult = @(Convert-JsonRecords $stopOutput | Where-Object { $_.event -in @('normal_ui_stopped','normal_ui_stop_cdp_port_retained','normal_ui_stop_refused') }) | Select-Object -Last 1
        if ($null -eq $stopResult -or $stopResult.event -ne 'normal_ui_stopped') { throw 'NORMAL_UI_WATCH_FINAL_STOP_FAILED' }
    }
}

Write-Output (ConvertTo-Json -Compress ([ordered]@{ event='normal_ui_watch_finished'; runtimeRoot=$runtimeRoot; outcome=$watchOutcome; stopped=$true }))
