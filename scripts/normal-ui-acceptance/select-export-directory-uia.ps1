[CmdletBinding()]
param(
  [Parameter(Mandatory)][ValidateRange(1, 2147483647)][int]$OwnerProcessId,
  [Parameter(Mandatory)][string]$ExpectedOwnerExe,
  [Parameter(Mandatory)][string]$AllowedRoot,
  [Parameter(Mandatory)][string]$Directory,
  [ValidateRange(1, 60)][int]$TimeoutSeconds = 15,
  [switch]$Select,
  [switch]$InspectOwnedWindows,
  [switch]$CaptureBaseline,
  [switch]$InspectNewWindows,
  [string]$BaselinePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-NoReparseDirectory {
  param([string]$Path, [string]$Label)
  $item = Get-Item -LiteralPath $Path -Force
  if (-not $item.PSIsContainer) { throw "UIA_EXPORT_${Label}_NOT_DIRECTORY" }
  $cursor = $item
  while ($null -ne $cursor) {
    if (($cursor.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "UIA_EXPORT_${Label}_REPARSE" }
    $parent = [IO.Directory]::GetParent($cursor.FullName)
    if ($null -eq $parent -or $parent.FullName -eq $cursor.FullName) { break }
    $cursor = Get-Item -LiteralPath $parent.FullName -Force
  }
  [IO.Path]::GetFullPath($item.FullName).TrimEnd([char[]]@([char]92, [char]47))
}

function Assert-DChild {
  param([string]$Root, [string]$Path, [string]$Label)
  if (-not $Path.StartsWith('D:\', [StringComparison]::OrdinalIgnoreCase)) { throw "UIA_EXPORT_${Label}_NOT_D" }
  $relative = [IO.Path]::GetRelativePath($Root, $Path)
  if ($relative -eq '..' -or $relative.StartsWith("..$([IO.Path]::DirectorySeparatorChar)") -or [IO.Path]::IsPathRooted($relative)) { throw "UIA_EXPORT_${Label}_OUTSIDE_ROOT" }
}

function Get-ProcessTreeIds {
  param([int]$RootProcessId)
  $children = @{}
  foreach ($process in Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId) {
    $key = [string]$process.ParentProcessId
    if (-not $children.ContainsKey($key)) { $children[$key] = [Collections.Generic.List[int]]::new() }
    $children[$key].Add([int]$process.ProcessId)
  }
  $result = [Collections.Generic.HashSet[int]]::new()
  $queue = [Collections.Generic.Queue[int]]::new(); $queue.Enqueue($RootProcessId)
  while ($queue.Count -gt 0) {
    $id = $queue.Dequeue(); if (-not $result.Add($id)) { continue }
    foreach ($child in @($children[[string]$id])) { $queue.Enqueue($child) }
  }
  $result
}

if (-not ('NormalUiExportNative' -as [type])) {
  Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class NormalUiExportNative {
  public const uint GW_OWNER = 4;
  [DllImport("user32.dll", SetLastError = true)] public static extern IntPtr GetWindow(IntPtr hWnd, uint uCmd);
  [DllImport("user32.dll", SetLastError = true)] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);
}
'@
}

function Get-OwnerChainIds {
  param([IntPtr]$Handle)
  $ids = [Collections.Generic.List[int]]::new(); $seen = [Collections.Generic.HashSet[long]]::new(); $current = $Handle
  while ($current -ne [IntPtr]::Zero -and $seen.Add($current.ToInt64())) {
    [uint32]$pid = 0; [void][NormalUiExportNative]::GetWindowThreadProcessId($current, [ref]$pid)
    if ($pid -gt 0) { $ids.Add([int]$pid) }
    $current = [NormalUiExportNative]::GetWindow($current, [NormalUiExportNative]::GW_OWNER)
  }
  $ids
}

function Test-DialogOwner {
  param([System.Windows.Automation.AutomationElement]$Window, [IntPtr]$NormalWindow)
  $handle = [IntPtr]$Window.Current.NativeWindowHandle
  if ($handle -eq [IntPtr]::Zero) { return $false }
  return [NormalUiExportNative]::GetWindow($handle, [NormalUiExportNative]::GW_OWNER) -eq $NormalWindow
}

function Get-NewWindowHandles {
  param([IntPtr]$NormalWindow, [Collections.Generic.HashSet[long]]$Baseline)
  $all = [System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition)
  @($all | Where-Object {
    $_.Current.ControlType -eq [System.Windows.Automation.ControlType]::Window -and
    -not $Baseline.Contains(([IntPtr]$_.Current.NativeWindowHandle).ToInt64()) -and
    (Test-DialogOwner $_ $NormalWindow)
  })
}

function Get-ExportDialog {
  param([IntPtr]$NormalWindow, [Collections.Generic.HashSet[long]]$Baseline)
  $title = '选择漫画页导出文件夹'
  $matches = @()
  foreach ($window in Get-NewWindowHandles $NormalWindow $Baseline) {
    if ($window.Current.Name -ne $title) { continue }
    try { [void](Get-ValueControl $window); [void](Get-ConfirmButton $window); $matches += $window } catch { }
  }
  if ($matches.Count -gt 1) { throw 'UIA_EXPORT_DIALOG_AMBIGUOUS' }
  if ($matches.Count -eq 1) { return $matches[0] }
  $null
}

function Get-ValueControl {
  param([System.Windows.Automation.AutomationElement]$Dialog)
  $controls = [Collections.Generic.List[System.Windows.Automation.AutomationElement]]::new()
  foreach ($type in @([System.Windows.Automation.ControlType]::Edit, [System.Windows.Automation.ControlType]::ComboBox)) {
    $condition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, $type)
    foreach ($control in $Dialog.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) {
      [System.Windows.Automation.ValuePattern]$pattern = $null
      if ($control.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$pattern) -and -not $pattern.Current.IsReadOnly -and $control.Current.Name -notmatch '搜索|search') { $controls.Add($control) }
    }
  }
  $address = @($controls | Where-Object { $_.Current.Name -match '地址|address|位置|location|文件夹|folder' -or $_.Current.AutomationId -in @('41477', '1001') })
  if ($address.Count -eq 1) { return $address[0] }
  if ($address.Count -eq 0 -and $controls.Count -eq 1) { return $controls[0] }
  throw 'UIA_EXPORT_DIRECTORY_FIELD_AMBIGUOUS'
}

function Get-ConfirmButton {
  param([System.Windows.Automation.AutomationElement]$Dialog)
  $condition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
  $buttons = @($Dialog.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object { @('选择文件夹', 'Select Folder', '确定', 'OK') -contains $_.Current.Name })
  if ($buttons.Count -ne 1) { throw 'UIA_EXPORT_CONFIRMATION_AMBIGUOUS' }
  [System.Windows.Automation.InvokePattern]$pattern = $null
  if (-not $buttons[0].TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) { throw 'UIA_EXPORT_CONFIRMATION_UNSUPPORTED' }
  [pscustomobject]@{ Button = $buttons[0]; Pattern = $pattern }
}

function Test-DialogOpen {
  param([IntPtr]$Handle, [IntPtr]$NormalWindow)
  foreach ($window in [System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition)) {
    if ([IntPtr]$window.Current.NativeWindowHandle -eq $Handle -and (Test-DialogOwner $window $NormalWindow)) { return $true }
  }
  $false
}

$root = Resolve-NoReparseDirectory $AllowedRoot 'ROOT'; $directory = Resolve-NoReparseDirectory $Directory 'DIRECTORY'; Assert-DChild $root $root 'ROOT'; Assert-DChild $root $directory 'DIRECTORY'
$owner = Get-Process -Id $OwnerProcessId -ErrorAction Stop
if (-not [string]::Equals([IO.Path]::GetFullPath($owner.Path), [IO.Path]::GetFullPath($ExpectedOwnerExe), [StringComparison]::OrdinalIgnoreCase)) { throw 'UIA_EXPORT_OWNER_EXE_MISMATCH' }
if (@($Select,$InspectOwnedWindows,$CaptureBaseline,$InspectNewWindows | Where-Object { $_ }).Count -gt 1) { throw 'UIA_EXPORT_MODE_CONFLICT' }
if (($CaptureBaseline -or $InspectNewWindows) -and [string]::IsNullOrWhiteSpace($BaselinePath)) { throw 'UIA_EXPORT_BASELINE_PATH_REQUIRED' }
if (-not $Select -and -not $InspectOwnedWindows -and -not $CaptureBaseline -and -not $InspectNewWindows) { [pscustomobject]@{mode='dry-run';ownerProcessId=$OwnerProcessId;directory=$directory;allowedRoot=$root;action='No dialog lookup or UI Automation action was performed.'}|ConvertTo-Json -Compress; exit 0 }

Add-Type -AssemblyName UIAutomationClient; Add-Type -AssemblyName UIAutomationTypes
$tree = Get-ProcessTreeIds $OwnerProcessId
if ($CaptureBaseline) {
  $baselineFile = [IO.Path]::GetFullPath($BaselinePath); Assert-DChild $root $baselineFile 'BASELINE'
  if (Test-Path -LiteralPath $baselineFile) { throw 'UIA_EXPORT_BASELINE_EXISTS' }
  $handles = @([System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition) | ForEach-Object { ([IntPtr]$_.Current.NativeWindowHandle).ToInt64() } | Where-Object { $_ -ne 0 })
  [pscustomobject]@{mode='window-baseline';ownerProcessId=$OwnerProcessId;handles=$handles;action='Only top-level HWND values were captured; no title or control data was read.'}|ConvertTo-Json -Compress | Set-Content -LiteralPath $baselineFile -Encoding utf8
  [pscustomobject]@{mode='window-baseline';baselinePath=$baselineFile;windowCount=$handles.Count;action='No dialog control was changed.'}|ConvertTo-Json -Compress; exit 0
}
$normalWindow = [IntPtr]$owner.MainWindowHandle
if ($normalWindow -eq [IntPtr]::Zero) { throw 'UIA_EXPORT_OWNER_WINDOW_MISSING' }
$baseline = [Collections.Generic.HashSet[long]]::new()
if ($Select -or $InspectNewWindows) {
  $baselineFile = [IO.Path]::GetFullPath($BaselinePath); Assert-DChild $root $baselineFile 'BASELINE'
  if (-not (Test-Path -LiteralPath $baselineFile -PathType Leaf)) { throw 'UIA_EXPORT_BASELINE_MISSING' }
  foreach ($handle in @((Get-Content -Raw -LiteralPath $baselineFile | ConvertFrom-Json).handles)) { [void]$baseline.Add([long]$handle) }
}
if ($InspectOwnedWindows) {
  $owned = @([System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition) | Where-Object { Test-DialogOwner $_ $normalWindow } | ForEach-Object { [pscustomobject]@{name=$_.Current.Name;className=$_.Current.ClassName;automationId=$_.Current.AutomationId;controlType=$_.Current.ControlType.ProgrammaticName} })
  [pscustomobject]@{mode='owned-window-inspection';ownerProcessId=$OwnerProcessId;windows=$owned;action='No directory field or confirmation button was changed.'}|ConvertTo-Json -Depth 4 -Compress; exit 0
}
if ($InspectNewWindows) {
  $items = @(Get-NewWindowHandles $normalWindow $baseline | ForEach-Object { $handle=[IntPtr]$_.Current.NativeWindowHandle;[pscustomobject]@{handle=$handle.ToInt64();name=$_.Current.Name;className=$_.Current.ClassName;automationId=$_.Current.AutomationId;controlType=$_.Current.ControlType.ProgrammaticName;ownerProcessIds=@(Get-OwnerChainIds $handle)} })
  [pscustomobject]@{mode='new-owned-window-inspection';ownerProcessId=$OwnerProcessId;windows=$items;action='Only newly appeared owned top-level metadata was read; no field or confirmation button was changed.'}|ConvertTo-Json -Depth 5 -Compress; exit 0
}

$deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds); $dialog = $null
while ([DateTime]::UtcNow -lt $deadline) { if ($dialog = Get-ExportDialog $normalWindow $baseline) { break }; Start-Sleep -Milliseconds 150; $tree = Get-ProcessTreeIds $OwnerProcessId }
if ($null -eq $dialog) { throw 'UIA_EXPORT_DIALOG_NOT_FOUND' }
$control = Get-ValueControl $dialog; [System.Windows.Automation.ValuePattern]$value = $null
if (-not $control.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$value)) { throw 'UIA_EXPORT_DIRECTORY_FIELD_UNSUPPORTED' }
$confirm = Get-ConfirmButton $dialog
$old = $value.Current.Value; $value.SetValue($directory)
if (-not [string]::Equals($value.Current.Value.TrimEnd([char[]]@([char]92,[char]47)), $directory, [StringComparison]::OrdinalIgnoreCase)) { try { $value.SetValue($old) } catch {}; throw 'UIA_EXPORT_DIRECTORY_VALUE_REJECTED' }
try { $confirm.Pattern.Invoke() } catch { try { $value.SetValue($old) } catch {}; throw 'UIA_EXPORT_CONFIRMATION_INVOKE_FAILED' }
$handle = [IntPtr]$dialog.Current.NativeWindowHandle; $closeDeadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
while ([DateTime]::UtcNow -lt $closeDeadline) { if (-not (Test-DialogOpen $handle $normalWindow)) { break }; Start-Sleep -Milliseconds 150 }
if (Test-DialogOpen $handle $normalWindow) { throw 'UIA_EXPORT_DIALOG_DID_NOT_CLOSE' }
[pscustomobject]@{mode='selected';ownerProcessId=$OwnerProcessId;directory=$directory;dialogTitle=$dialog.Current.Name;action='UI Automation selected one D-only directory in one owned dialog and observed dialog close.'}|ConvertTo-Json -Compress
