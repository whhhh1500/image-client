# 读取 .env 的值并通过 PUT /api/v1/system/config 写入运行中的应用。
param(
  [string]$BaseUrl = "http://127.0.0.1:8123",
  [string]$ImageModel = "gpt-image-2",
  [string]$VideoModel = "grok-imagine-video",
  [string]$LlmModel = "gemini-3.7-flash"
)

$envMap = @{}
Get-Content "$PSScriptRoot\..\.env" | ForEach-Object {
  if ($_ -match '^\s*([A-Za-z0-9_-]+)\s*=\s*(.*)\s*$') { $envMap[$matches[1]] = $matches[2].Trim() }
}

$cfg = @{
  imageApiUrl   = $envMap["image-api-url"]; imageApiKey = $envMap["image-api-key"]; imageApiModel = $ImageModel
  videoApiUrl   = $envMap["video-api-url"]; videoApiKey = $envMap["video-api-key"]; videoApiModel = $VideoModel
  llmApiUrl     = $envMap["llm-api-url"];   llmApiKey   = $envMap["llm-api-key"];   llmApiModel   = $LlmModel
  outputDir     = ""
}

$r = Invoke-RestMethod -Method Put -Uri "$BaseUrl/api/v1/system/config" `
  -ContentType "application/json" -Body ($cfg | ConvertTo-Json) -TimeoutSec 30
"llm=$($r.llmReady) img=$($r.imageReady) vid=$($r.videoReady) source=$($r.source)"
