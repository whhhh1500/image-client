# 逐个直传视频模型创建任务（文档协议），首个成功即完成下载并停止。
param(
  [string]$BaseUrl = "http://127.0.0.1:8123"
)

$models = @(
  "grok-imagine-video",
  "grok-imagine-video-1.5-preview",
  "kling-video-v3",
  "kling-video-v3-omni",
  "kling-video-v3-turbo",
  "seedance2.5"
)
$prompt = "雨夜城市街道，一辆重型泥头车冲下陡坡驶向斑马线，车灯撕裂雨幕，地面水花飞溅，电影感慢镜头"

foreach ($m in $models) {
  Write-Host ">> 尝试 $m"
  $body = @{ prompt = $prompt; duration_s = 5; model = $m } | ConvertTo-Json
  try {
    $r = Invoke-RestMethod -Method Post -Uri "$BaseUrl/api/v1/media/videos/generations" `
      -ContentType "application/json; charset=utf-8" `
      -Body ([System.Text.Encoding]::UTF8.GetBytes($body)) `
      -TimeoutSec 600 -SkipHttpErrorCheck
      if ($r.assets -and $r.assets.Count -gt 0 -and $r.assets[-1].path) {
        Write-Host "[SUCCESS] $m -> $($r.assets[-1].path)（$($r.assets.Count) 段）"
      exit 0
    } else {
      $msg = if ($r.error) { $r.error | ConvertTo-Json -Compress -Depth 3 } else { ($r | ConvertTo-Json -Compress) }
      Write-Host "[FAIL] $m :: $msg"
    }
  } catch {
    Write-Host "[FAIL] $m :: $_"
  }
}
exit 1
