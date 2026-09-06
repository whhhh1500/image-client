# 整章批量生产：文本链（带缓存）→ 逐镜出图 → 可选逐镜出视频。
param(
  [string]$InputFile = "ddd\新建文本文档.txt",
  [string]$BaseUrl = "http://127.0.0.1:8123",
  [string]$LlmModel = "gemini-3.7-flash",
  [string]$ImageModel = "gpt-image-2",
  [string]$ImageQuality = "medium",   # low / medium / high
  [string]$ImageSize = "1024x1024",
  [string]$VideoModel = "grok-imagine-video-1.5-preview",
  [int]$VideoSeconds = 5,
  [int]$MaxShots = 0,                 # 0 = 全部镜头
  [switch]$WithVideo,
  [switch]$ChainReference             # 用第 1 镜成图作为后续镜头参考（角色一致性）
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$isWindowsPlatform = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)
if (-not [IO.Path]::IsPathRooted($InputFile)) { $InputFile = Join-Path $repoRoot $InputFile }
if (-not (Test-Path -LiteralPath $InputFile)) { throw "输入文件不存在: $InputFile" }
$chapter = [IO.Path]::GetFileNameWithoutExtension($InputFile)

function Post-Json([string]$path, $body, [int]$timeoutSec = 600) {
  $json = [System.Text.Encoding]::UTF8.GetBytes(($body | ConvertTo-Json -Depth 8))
  Invoke-RestMethod -Method Post -Uri "$BaseUrl$path" `
    -ContentType "application/json; charset=utf-8" -Body $json -TimeoutSec $timeoutSec
}

function Extract-Json([string]$text) {
  $s = $text.IndexOf("{"); $e = $text.LastIndexOf("}")
  if ($s -lt 0 -or $e -le $s) { return $null }
  try { $text.Substring($s, $e - $s + 1) | ConvertFrom-Json } catch { $null }
}

# 1) 确保应用与配置就绪
$launched = $null
$ready = $false
for ($i = 0; $i -lt 3; $i++) {
  try { $h = Invoke-RestMethod "$BaseUrl/api/v1/health" -TimeoutSec 2; if ($h.status -eq "ok") { $ready = $true; break } } catch {}
  Start-Sleep 1
}
if (-not $ready) {
  $binary = if ($isWindowsPlatform) { "image-client.exe" } else { "image-client" }
  $exePath = Join-Path $repoRoot "src-tauri/target/debug/$binary"
  if (-not (Test-Path -LiteralPath $exePath)) { throw "调试可执行文件不存在，请先运行 pnpm tauri build --no-bundle: $exePath" }
  $startArgs = @{ FilePath = $exePath; WorkingDirectory = $repoRoot; PassThru = $true }
  if ($isWindowsPlatform) { $startArgs.WindowStyle = "Hidden" }
  $launched = Start-Process @startArgs
  for ($i = 0; $i -lt 30; $i++) {
    try { $h = Invoke-RestMethod "$BaseUrl/api/v1/health" -TimeoutSec 2; if ($h.status -eq "ok") { $ready = $true; break } } catch {}
    Start-Sleep 1
  }
}
if (-not $ready) { throw "REST 服务未就绪" }
& "$PSScriptRoot\apply-config.ps1" -BaseUrl $BaseUrl -ImageModel $ImageModel -VideoModel $VideoModel -LlmModel $LlmModel | Out-Null
$runtimeConfig = Invoke-RestMethod "$BaseUrl/api/v1/system/config" -TimeoutSec 10
$assetsRoot = $runtimeConfig.outputDir
if (-not $assetsRoot) { throw "运行配置未返回输出目录" }
$outDir = Join-Path (Join-Path $assetsRoot "剧本") $chapter
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

# 2) 文本链（缓存复用，避免重复烧 token）
$shotsFile = Join-Path $outDir "shots.json"
if (Test-Path $shotsFile) {
  $shots = @(Get-Content $shotsFile -Raw -Encoding UTF8 | ConvertFrom-Json)
  Write-Host "[cache] 复用已缓存分镜 $($shots.Count) 镜"
} else {
  $story = Get-Content -Raw -Encoding UTF8 $InputFile
  # 链路输入：导演收原文；编剧收导演纲领；一致性收剧本；分镜收剧本；
  # 质检收 剧本+分镜+锚 全量，避免只见上一步输出。
  $director = (Post-Json "/api/v1/agents/director/runs" @{ input = $story; model = $LlmModel } 300).result
  Set-Content -Path (Join-Path $outDir "director.md") -Value $director -Encoding UTF8
  Write-Host "[llm] director.md $($director.Length) 字"

  $scriptText = (Post-Json "/api/v1/agents/writer/runs" @{ input = $director; model = $LlmModel } 300).result
  Set-Content -Path (Join-Path $outDir "script.md") -Value $scriptText -Encoding UTF8
  Write-Host "[llm] script.md $($scriptText.Length) 字"

  $anchors = (Post-Json "/api/v1/agents/consistency/runs" @{ input = $scriptText; model = $LlmModel } 300).result
  Set-Content -Path (Join-Path $outDir "anchors.md") -Value $anchors -Encoding UTF8
  Write-Host "[llm] anchors.md $($anchors.Length) 字"

  $board = (Post-Json "/api/v1/agents/storyboard/runs" @{ input = $scriptText; model = $LlmModel } 300).result
  Set-Content -Path (Join-Path $outDir "storyboard.md") -Value $board -Encoding UTF8
  Write-Host "[llm] storyboard.md $($board.Length) 字"

  $qcInput = (@($scriptText, $board, $anchors) | Where-Object { $_ }) -join "`n`n---`n`n"
  $qc = (Post-Json "/api/v1/agents/qc/runs" @{ input = $qcInput; model = $LlmModel } 300).result
  Set-Content -Path (Join-Path $outDir "qc.md") -Value $qc -Encoding UTF8
  Write-Host "[llm] qc.md $($qc.Length) 字"

  $parsed = Extract-Json $board
  if (-not $parsed -or -not $parsed.shots) { throw "分镜 JSON 解析失败，检查 $outDir\storyboard.md" }
  $shots = @($parsed.shots)
  $shots | ConvertTo-Json -Depth 6 | Set-Content -Path $shotsFile -Encoding UTF8
  Write-Host "[llm] 解析 $($shots.Count) 镜，已缓存"
}

$total = if ($MaxShots -gt 0) { [Math]::Min($MaxShots, $shots.Count) } else { $shots.Count }

# 3) 逐镜出图（失败重试一次，单镜失败不中断）
$imagePaths = @()
$refPath = ""
for ($i = 0; $i -lt $total; $i++) {
  $shot = $shots[$i]
  $prompt = $shot.prompt
  if (-not $prompt) {
    $prompt = ($shot | Get-Member -MemberType NoteProperty | ForEach-Object { $shot.($_.Name) }) -join ", "
  }
  $body = @{ prompt = $prompt; size = $ImageSize; quality = $ImageQuality; model = $ImageModel }
  if ($ChainReference -and $refPath) { $body.referencePath = $refPath }
  $done = $null
  for ($attempt = 1; $attempt -le 2; $attempt++) {
    try { $r = Post-Json "/api/v1/media/images/generations" $body 300; $done = $r.assets[0].path; break }
    catch { Write-Host "[img $attempt/2] #$($i+1) FAIL: $_" }
  }
  if ($done) {
    $imagePaths += $done
    if (-not $refPath) { $refPath = $done }
    Write-Host "[img] #$($i+1)/$total OK"
  } else {
    $imagePaths += ""
    Write-Host "[img] #$($i+1)/$total 跳过（连续失败）"
  }
  Start-Sleep -Milliseconds 500
}

# 4) 可选：逐镜出视频
$videoPaths = @()
if ($WithVideo) {
  for ($i = 0; $i -lt $total; $i++) {
    if (-not $imagePaths[$i]) { $videoPaths += ""; continue }
    $prompt = $shots[$i].prompt
    if (-not $prompt) { $prompt = "镜头 $($i+1)" }
    $body = @{ prompt = $prompt; duration_s = $VideoSeconds; model = $VideoModel }
    $done = $null
    try { $r = Post-Json "/api/v1/media/videos/generations" $body 600; $done = $r.assets[-1].path } catch { Write-Host "[vid] #$($i+1) FAIL: $_" }
    $videoPaths += $done
    if ($done) { Write-Host "[vid] #$($i+1)/$total OK" }
  }
}

# 5) 清单与汇总
$manifest = @{ chapter = $chapter; imageModel = $ImageModel; videoModel = $VideoModel; shots = @() }
for ($i = 0; $i -lt $total; $i++) {
  $entry = @{ shotNo = $shots[$i].shotNo; prompt = $shots[$i].prompt; image = $imagePaths[$i] }
  if ($WithVideo) { $entry.video = $videoPaths[$i] }
  $manifest.shots += $entry
}
$manifest | ConvertTo-Json -Depth 6 | Set-Content -Path (Join-Path $outDir "manifest.json") -Encoding UTF8

$okImg = @($imagePaths | Where-Object { $_ }).Count
Write-Host "`n===== 汇总 ====="
Write-Host "章节: $chapter（$total/$($shots.Count) 镜）"
Write-Host "文本产物: $outDir\{director,script,anchors,storyboard,qc}.md"
Write-Host "图像: $okImg/$total 张 -> $assetsRoot\图片\api\"
if ($WithVideo) {
  $okVid = @($videoPaths | Where-Object { $_ }).Count
  Write-Host "视频: $okVid/$total 段 -> $assetsRoot\视频\api\"
}
Write-Host "清单: $outDir\manifest.json"
if ($launched) { Stop-Process -Id $launched.Id -Force -ErrorAction SilentlyContinue }
