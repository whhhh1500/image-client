[CmdletBinding()]
param(
  [switch]$Launch,
  [switch]$Stop,
  [Parameter(Mandatory)][string]$RuntimeRoot,
  [ValidateRange(1025,65535)][int]$CdpPort = 61991
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$testRoot = [IO.Path]::GetFullPath((Join-Path $projectRoot '.test-tmp'))
$root = [IO.Path]::GetFullPath($RuntimeRoot)
function Assert-Child([string]$path,[string]$code) {
  $full=[IO.Path]::GetFullPath($path); $prefix=$testRoot.TrimEnd([char]92,[char]47)+[IO.Path]::DirectorySeparatorChar
  if (-not $full.StartsWith($prefix,[StringComparison]::OrdinalIgnoreCase) -or -not $full.StartsWith('D:',[StringComparison]::OrdinalIgnoreCase)) { throw "$code`_OUTSIDE_D_TEST_ROOT" }
  for($cursor=$full;;$cursor=[IO.Path]::GetDirectoryName($cursor)) { if(Test-Path -LiteralPath $cursor){if(((Get-Item -LiteralPath $cursor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)-ne 0){throw "$code`_REPARSE"}}; $parent=[IO.Path]::GetDirectoryName($cursor); if($parent -eq $cursor -or !$parent){break} }
  return $full
}
function Assert-ProjectFile([string]$path,[string]$code) {
  $full=[IO.Path]::GetFullPath($path);$prefix=$projectRoot.TrimEnd([char]92,[char]47)+[IO.Path]::DirectorySeparatorChar
  if(-not $full.StartsWith($prefix,[StringComparison]::OrdinalIgnoreCase) -or -not $full.StartsWith('D:',[StringComparison]::OrdinalIgnoreCase)){throw "$code`_OUTSIDE_D_PROJECT"}
  for($cursor=$full;;$cursor=[IO.Path]::GetDirectoryName($cursor)){if(Test-Path -LiteralPath $cursor){if(((Get-Item -LiteralPath $cursor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)-ne 0){throw "$code`_REPARSE"}};$parent=[IO.Path]::GetDirectoryName($cursor);if($parent -eq $cursor -or !$parent){break}}
  return $full
}
function Tree([int]$processId) { $set=[Collections.Generic.HashSet[int]]::new();$q=[Collections.Generic.Queue[int]]::new();$q.Enqueue($processId);while($q.Count){$p=$q.Dequeue();if(!$set.Add($p)){continue};Get-CimInstance Win32_Process -Filter "ParentProcessId=$p" -ErrorAction SilentlyContinue|ForEach-Object{$q.Enqueue([int]$_.ProcessId)}};return @($set) }
$root=Assert-Child $root 'RECOVERY_RUNTIME';$statusPath=Join-Path $root 'recovery-status.json';$contextPath=Join-Path $root 'recovery-context.json'
if($Launch -eq $Stop){throw 'RECOVERY_EXACTLY_ONE_MODE_REQUIRED'}
if($Stop){
  if(!(Test-Path -LiteralPath $statusPath)){throw 'RECOVERY_STATUS_MISSING'};$s=Get-Content -Raw -LiteralPath $statusPath|ConvertFrom-Json
  foreach($treeProcessId in @(Tree ([int]$s.normalPid))){Stop-Process -Id $treeProcessId -Force -ErrorAction SilentlyContinue}
  $deadline=[DateTimeOffset]::UtcNow.AddSeconds(8);do{Start-Sleep -Milliseconds 200;$left=@(Get-Process -Id ([int]$s.normalPid) -ErrorAction SilentlyContinue)}while($left.Count -and [DateTimeOffset]::UtcNow -lt $deadline)
  foreach($name in @('remainingPids','cdpPortReleased','configurationCleanup')){if($null -eq $s.PSObject.Properties[$name]){$s|Add-Member -NotePropertyName $name -NotePropertyValue $null}}
  $s.status='stopped';$s.remainingPids=@($left|ForEach-Object Id);$s.cdpPortReleased=-not (Get-NetTCPConnection -State Listen -LocalPort ([int]$s.cdpPort) -ErrorAction SilentlyContinue);$s.configurationCleanup='not-created-empty-provider-config';$s|ConvertTo-Json -Depth 5|Set-Content -LiteralPath $statusPath -Encoding utf8
  if($left.Count -or !$s.cdpPortReleased){throw 'RECOVERY_STOP_INCOMPLETE'};exit 0
}
if(!(Test-Path -LiteralPath $contextPath)){throw 'RECOVERY_CONTEXT_MISSING'};$c=Get-Content -Raw -LiteralPath $contextPath|ConvertFrom-Json
if($c.schemaVersion -ne 'normal-ui-recovery.v1'){throw 'RECOVERY_CONTEXT_VERSION'}
foreach($name in @('databasePath','scopePath','normalExecutablePath','normalSha256')){if([string]::IsNullOrWhiteSpace([string]$c.$name)){throw "RECOVERY_CONTEXT_$name`_MISSING"}}
$db=Assert-Child ([string]$c.databasePath) 'RECOVERY_DB';$scope=Assert-Child ([string]$c.scopePath) 'RECOVERY_SCOPE';$sourceExe=Assert-ProjectFile ([string]$c.normalExecutablePath) 'RECOVERY_EXE';if(!(Test-Path -LiteralPath $db) -or !(Test-Path -LiteralPath $scope) -or !(Test-Path -LiteralPath $sourceExe)){throw 'RECOVERY_INPUT_MISSING'}
if((Get-FileHash -LiteralPath $sourceExe -Algorithm SHA256).Hash.ToUpperInvariant() -ne [string]$c.normalSha256){throw 'RECOVERY_EXE_HASH_MISMATCH'}
if(Get-NetTCPConnection -State Listen -LocalPort $CdpPort -ErrorAction SilentlyContinue){throw 'RECOVERY_CDP_PORT_BUSY'}
$config=Get-Content -Raw -LiteralPath (Join-Path $root 'data\backend-config.json')|ConvertFrom-Json;foreach($key in @('image_api_url','image_api_key','llm_api_url','llm_api_key','video_api_url','video_api_key')){if([string]$config.$key){throw 'RECOVERY_PROVIDER_CONFIG_NOT_EMPTY'}}
$bin=Join-Path $root 'bin';$temp=Join-Path $root 'tmp';New-Item -ItemType Directory -Force -Path $bin,$temp|Out-Null;$copied=Join-Path $bin 'image-client.exe';Copy-Item -LiteralPath $sourceExe -Destination $copied -Force;if((Get-FileHash -LiteralPath $copied -Algorithm SHA256).Hash.ToUpperInvariant() -ne [string]$c.normalSha256){throw 'RECOVERY_COPIED_EXE_HASH_MISMATCH'}
$info=[Diagnostics.ProcessStartInfo]::new();$info.FileName=$copied;$info.WorkingDirectory=$bin;$info.UseShellExecute=$false;$info.CreateNoWindow=$false;$info.WindowStyle=[Diagnostics.ProcessWindowStyle]::Normal;$info.Environment.Clear();foreach($n in @('SystemRoot','WINDIR','SystemDrive','ComSpec','PATH')){$info.Environment[$n]=[Environment]::GetEnvironmentVariable($n,'Process')};$info.Environment['TEMP']=$temp;$info.Environment['TMP']=$temp;$info.Environment['IMAGE_CLIENT_DATA_DIR']=Join-Path $root 'data';$info.Environment['WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS']="--remote-debugging-address=127.0.0.1 --remote-debugging-port=$CdpPort";foreach($n in @('IMAGE_API_KEY','LLM_API_KEY','VIDEO_API_KEY','OPENAI_API_KEY','ANTHROPIC_API_KEY')){$info.Environment[$n]=''}
$process=[Diagnostics.Process]::Start($info);if(!$process){throw 'RECOVERY_NORMAL_START_FAILED'}
[ordered]@{schemaVersion='normal-ui-recovery-live.v1';status='running';runtimeRoot=$root;normalPid=$process.Id;normalExecutablePath=$copied;normalSha256=[string]$c.normalSha256;cdpPort=$CdpPort;scopePath=$scope;databasePath=$db;stopScriptPath=$PSCommandPath;startedAtUtc=[DateTimeOffset]::UtcNow.ToString('O')}|ConvertTo-Json|Set-Content -LiteralPath $statusPath -Encoding utf8
Write-Output $statusPath
