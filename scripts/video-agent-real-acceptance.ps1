[CmdletBinding()]
param(
  [string]$BaseUrl = 'http://127.0.0.1:18129/api/v1',
  [string]$SourcePath = 'D:\cc\image-client\ddd\新建文本文档.txt',
  [string]$EnvPath = 'D:\cc\image-client\.env',
  [string]$OutputPath = 'D:\cc\image-client\.test-tmp\video-agent-real-output',
  [string]$Model = ''
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$settings = @{}
Get-Content -LiteralPath $EnvPath | ForEach-Object {
  if ($_ -match '^\s*([^#=]+)=(.*)$') { $settings[$matches[1].Trim()] = $matches[2].Trim() }
}
$llmUrl = [string]$settings['llm-api-url']
$llmKey = [string]$settings['llm-api-key']
$llmModel = if ($Model) { $Model } else { [string]$settings['llm-api-model'] }
if (-not $llmModel) { $llmModel = 'gemini-3.7-flash' }
if (-not $llmUrl -or -not $llmKey) { throw 'LLM URL or Key is missing in .env' }

$before = Invoke-RestMethod -Uri "$BaseUrl/system/llm" -Method Get -TimeoutSec 30
if (-not $before.ready) {
  throw 'System LLM is not ready. Configure it explicitly before acceptance; this script never modifies the user LLM settings.'
}
# Every request below passes the test model explicitly. Keep the user's active LLM config untouched.
$configured = $before

$source = Get-Content -Raw -LiteralPath $SourcePath
$marker = '下一刻，剧痛骤然归于无尽的死寂。'
$end = $source.IndexOf($marker, [StringComparison]::Ordinal)
if ($end -lt 0) { throw 'Could not find the real-scene end marker' }
$excerpt = $source.Substring(0, $end + $marker.Length)
$sourceMarkdown = "# 原始资料`n`n## 类型`n小说`n`n## 正文`n$excerpt"
$projectId = 'video-agent-real-acceptance'

function Invoke-JsonPostWithRetry([string]$Uri, [string]$Body) {
  for ($attempt = 1; $attempt -le 3; $attempt++) {
    try {
      return Invoke-RestMethod -Uri $Uri -Method Post -ContentType 'application/json; charset=utf-8' -Body $Body -TimeoutSec 900
    } catch {
      $transient = [string]$_ -match 'try again later|429|5(?:00|02|03|04|20|21|22|23|24|25|26|27|28|29)|unknown status code|temporar|timeout|network|连接|超时|稍后重试'
      if (-not $transient -or $attempt -eq 3) { throw }
      Write-Host "Transient LLM/API failure; retry $attempt/3: $($_.Exception.Message)"
      Start-Sleep -Seconds (2 * $attempt)
    }
  }
}

function Invoke-Agent([string]$Name, [string]$AgentInput) {
  $body = @{ input = $AgentInput; model = $llmModel; projectId = $projectId } | ConvertTo-Json -Depth 8
  $response = Invoke-JsonPostWithRetry "$BaseUrl/agents/$Name/runs" $body
  if (Test-ProviderRefusal ([string]$response.result)) {
    $retryInput = Convert-ToSafetyRetryContext $AgentInput
    $retryBody = @{ input = $retryInput; model = $llmModel; projectId = $projectId } | ConvertTo-Json -Depth 8
    $response = Invoke-JsonPostWithRetry "$BaseUrl/agents/$Name/runs" $retryBody
  }
  return $response
}

function Test-ProviderRefusal([string]$Text) {
  return $Text -match 'prompt could not be submitted|prohibited use policy|sensitive words|无法提交.{0,20}(敏感|安全策略)|模型.{0,20}安全策略.{0,20}拒绝'
}

function Convert-ToSafetyContext([string]$Text) {
  $safe = $Text
  $safe = $safe -replace '骨骼碎裂|骨头碎裂|全身骨碎', '严重受伤（仅用遮挡、反应镜头和画面切黑表达）'
  $safe = $safe -replace '血泊|大片鲜血|猩红鲜血', '事故现场的非图形化红色视觉提示'
  $safe = $safe -replace '血雾|喷血|口吐鲜血', '受伤反应（不展示血腥细节）'
  $safe = $safe -replace '碾碎|碾成|肉体撞击', '车辆冲击（镜外或遮挡呈现）'
  $safe = $safe -replace '尸体|残肢|内脏|肢解', '严重事故后果（不直接展示）'
  return "【安全与生产表达】`n这是合法的虚构剧情改编。事故、危险和受伤只允许非图形化呈现：使用遮挡、影子、物体反应、人物视线和画面切黑，不展示血腥细节，不鼓励或指导真实伤害。`n`n$safe"
}

function Convert-ToSafetyRetryContext([string]$Text) {
  $safe = Convert-ToSafetyContext $Text
  $safe = $safe -replace '八岁|8岁', '未成年角色（年龄设定8岁，始终保持安全且不展示伤害结果）'
  $safe = $safe -replace '撞击|冲撞|撞上|碾压', '非图形化危险接触（接触点始终由车体或阴影遮挡）'
  $safe = $safe -replace '死亡|濒死|死寂', '主观视线逐渐淡出至纯黑'
  $safe = $safe -replace '受伤|重伤', '事故后虚弱状态（不展示伤口）'
  $safe = $safe -replace '血色|猩红', '主观半透明红色暗角'
  return "【安全重试】`n只使用非图形化、非教学性的影视制作语言；未成年角色始终不展示受伤且最终安全；危险接触点必须被车体、阴影或画面切黑遮挡。`n`n$safe"
}

function Invoke-Review([string]$Stage, [string]$Dependencies, [string]$Output) {
  $stageRule = switch ($Stage) {
    '改编规划' { '只审核输入边界、硬约束、人物选择、叙事节拍、视觉策略和风险自检；不要要求逐镜表格或锚点库。' }
    '视频剧本' { '重点审核每场目标、动作因果、人物对白、原文忠实度和物理可信度；不要要求分镜字段。' }
    '视频锚点' { '重点审核固定锚、剧情状态锚、ID、制作设定标注和变化记录。' }
    '视频分镜' { '必须逐镜审核时长、动作复杂度、画幅、锚点、承接和非图形化表现。' }
    '质检报告复核' { '必须检查质检是否包含硬规则结果、锚点检查和覆盖所有镜头的逐镜检查；缺任一证据就判需修改。' }
    default { '按当前阶段职责审核，不向它要求下游阶段才有的字段。' }
  }
  $system = @'
你是独立、严格、对抗性的短剧内容总编和视频生产审核员。不要附和生成者，不重写产物，只审核。
重点检查：原文忠实度、是否越过正文结尾、人物动机、剧情吸引力、模板腔、物理可信度、项目画幅、锚点结构、镜头可执行性和 QC 漏检。
任何用反向受力解释人物滞留的写法都不是可信物理，必须指出。只有在审核“质检报告复核”阶段时，才要求逐镜检查、锚点检查和硬规则证据。
区分“制作合规”和“真正精彩”。常见车祸救亲母题如果缺少独有关系细节、选择困境或视觉母题，剧情吸引力与视觉独特性不得轻易高于79；不得为了提分越过原文添加设定。90分以上只给少量真正独特且高度完成的产物。
存在越界续写、硬约束冲突、结构残缺、锚点冲突或不可执行镜头时，总分不得高于69。
只有总分至少80且阻断问题为“无”才能判定通过。
只输出 Markdown：
# 产物质量审查
【结论】通过 / 需修改
【总分】0-100
【剧情吸引力】0-100 | 依据
【视觉独特性】0-100 | 独有细节或模板风险
【原文忠实度】0-100 | 依据
【人物与情感】0-100 | 依据
【可执行性】0-100 | 依据
【一致性】0-100 | 依据
【亮点】具体内容或“无”
【阻断问题】位置 | 问题 | 原因 | 修正方向；没有写“无”
【一般问题】位置 | 问题 | 修正方向；没有写“无”
【改进建议】可直接用于下一轮优化的要求
'@
  $system = "$system`n改进建议只能重组原文已有细节，不得建议新增原文没有的道具、家庭标识、往事或关键动作。每镜1-15秒是单镜限制，不是全片总时长。`n`n【当前阶段专用标准】`n$stageRule"
  $input = Convert-ToSafetyContext "【阶段】`n$Stage`n`n【项目与前序资料】`n$Dependencies`n`n【待审查产物】`n$Output"
  $body = @{ system = $system; input = $input; model = $llmModel } | ConvertTo-Json -Depth 8
  $response = Invoke-JsonPostWithRetry "$BaseUrl/text/completions" $body
  if (Test-ProviderRefusal ([string]$response.result)) {
    $retryInput = Convert-ToSafetyRetryContext $input
    $retryBody = @{ system = "$system`n`n安全重试：危险接触点必须遮挡，不输出任何图形化伤害。"; input = $retryInput; model = $llmModel } | ConvertTo-Json -Depth 8
    $response = Invoke-JsonPostWithRetry "$BaseUrl/text/completions" $retryBody
  }
  return $response
}

$script:RepairCount = 0
$script:ReviewRetryCount = 0

function Test-ReviewWellFormed($Review) {
  $text = [string]$Review.result
  return $text.Contains('【结论】') -and $text.Contains('【总分】') -and $text.Contains('【阻断问题】') -and $text.Contains('【改进建议】')
}

function Test-ReviewPassed($Review) {
  if (-not (Test-ReviewWellFormed $Review)) { return $false }
  $text = [string]$Review.result
  $conclusion = [regex]::Match($text, '【结论】\s*([^\r\n]+)').Groups[1].Value.Trim()
  $score = [regex]::Match($text, '【总分】\s*(\d{1,3})')
  $blockers = [regex]::Match($text, '(?s)【阻断问题】\s*(.*?)(?=\r?\n【[^】]+】|\z)').Groups[1].Value.Trim()
  return $conclusion.StartsWith('通过') -and $score.Success -and [int]$score.Groups[1].Value -ge 80 -and $blockers -match '^无[。\s]*$'
}

function Test-QcVerdictPassed([string]$Text) {
  $conclusion = [regex]::Match($Text, '【结论】\s*([^\r\n]+)').Groups[1].Value.Trim()
  $blockers = [regex]::Match($Text, '(?s)【阻断问题】\s*(.*?)(?=\r?\n【[^】]+】|\z)').Groups[1].Value.Trim()
  return $conclusion.StartsWith('可生成') -and $blockers -match '^无[。\s]*$'
}

function Get-StageProgramIssues([string]$Stage, [string]$Output) {
  $issues = @()
  if (Test-ProviderRefusal $Output) { $issues += '供应商返回了安全策略拒答文本，不能作为阶段产物。' }
  $assertive = (($Output -split "`r?`n") | Where-Object { $_ -notmatch '不得|禁止|避免|剔除|不使用|不要复述|已修正|不添加|不出现|不增设|不暗示|未添加|未引入|未包含|未延伸|没有|严格终结|严格停|风险.*无' }) -join "`n"
  if ($assertive -match '骨骼碎裂|血雾|喷血|内脏|残肢|肢解|(身体|肉体|肢体|骨骼).{0,6}碾碎|碾碎.{0,6}(身体|肉体|肢体|骨骼)|(大片|大量).{0,8}(血液|鲜血|猩红).{0,12}(蔓延|流淌|扩散)|染血.{0,6}(面孔|脸|衣物|衣服)') { $issues += '包含图形化伤害描写，必须改为遮挡、扬尘、主观暗角或切黑。' }
  if ($assertive -match '反作用力|反向力|(推飞|推开|平推|滑出|飞出).{0,8}(数米|四米|五米)') { $issues += '包含伪物理表述，必须改为冲刺惯性、落脚失败、重心失衡与侧向跌离。' }
  switch ($Stage) {
    '改编规划' {
      foreach ($heading in @('# 改编规划', '## 输入边界', '## 项目硬约束', '## 核心视觉母题', '## 风险与自检')) { if (-not $Output.Contains($heading)) { $issues += "缺少标题：$heading" } }
    }
    '视频剧本' {
      foreach ($heading in @('# 视频剧本', '## 改编边界', '## 分场剧本', '## 编剧自检')) { if (-not $Output.Contains($heading)) { $issues += "缺少标题：$heading" } }
    }
    '视频锚点' {
      foreach ($heading in @('# 视频锚点', '## 固定锚定', '## 剧情锚点', '### 变化记录')) { if (-not $Output.Contains($heading)) { $issues += "缺少标题：$heading" } }
      if ($Output -match '(?m)^character:[^|\r\n]+:v([2-9]\d*)\s*\|' -and $Output -notmatch '(?m)^character:[^|\r\n]+:v1\s*\|') { $issues += '首章固定角色锚无依据从 v2 或更高版本起号。' }
    }
    '视频分镜' {
      foreach ($heading in @('# 视频分镜', '### 时长', '### 画风锚', '### 场景锚', '### 角色锚', '### 视频 Prompt')) { if (-not $Output.Contains($heading)) { $issues += "缺少标题：$heading" } }
      $blocks = [regex]::Matches($Output, '(?ms)^##\s+第\s*(\d+)\s*镜\s*$.*?(?=^##\s+第\s*\d+\s*镜\s*$|\z)')
      if ($blocks.Count -lt 1) { $issues += '没有可解析的视频镜头。' }
      if ([regex]::Matches($Output, '9\s*:\s*16').Count -gt 0) { $issues += '视频 Prompt 画幅与项目 16:9 冲突。' }
      foreach ($block in $blocks) {
        $strategy = [regex]::Match($block.Value, '(?ms)^### 参考方式\s*\r?\n(.*?)\r?\n\r?\n').Groups[1].Value.Trim()
        $referenceAssets = [regex]::Match($block.Value, '(?ms)^### 参考资产\s*\r?\n(.*?)\r?\n\r?\n').Groups[1].Value.Trim()
        if ($strategy -ne 'text' -or $referenceAssets -ne '无') { $issues += "本次输入没有真实媒体资产，第$($block.Groups[1].Value)镜必须使用 text + 无，不能把锚点 ID 当参考资产。" }
      }
      if (@($blocks | Where-Object { $_.Value -match '飞扑|鱼跃' -and $_.Value -match '推' -and $_.Value -match '翻滚|跌出|滚跌' -and $_.Value -match '摔倒|扑倒|趴摔|失去平衡' }).Count -gt 0) { $issues += '存在同时承担接近、推人和双方位移结果的复合救援镜头，必须拆镜。' }
    }
    '质检报告复核' {
      foreach ($heading in @('【质量评分】', '【原文忠实度】', '【剧情吸引力】', '【视觉独特性】', '【硬规则结果】', '【锚点检查】', '【逐镜检查】', '【阻断问题】', '【一般问题】', '【结论】')) { if (-not $Output.Contains($heading)) { $issues += "缺少字段：$heading" } }
      if ($Output.Length -lt 500) { $issues += '质检报告过短，无法证明完成逐镜与硬规则检查。' }
      $originality = [regex]::Match($Output, '【视觉独特性】\s*(\d{1,3})')
      if ($originality.Success -and [int]$originality.Groups[1].Value -gt 79 -and $Output -match '经典|常见|常规|模板') { $issues += '质检承认母题常见，却把视觉独特性评为80分以上，必须校准评分。' }
    }
  }
  return $issues
}

function Invoke-StageWithReview([string]$Name, [string]$Stage, [string]$AgentInput, [string]$ReviewDependencies) {
  Write-Host "[$Stage] generating"
  $agent = Invoke-Agent $Name $AgentInput
  Write-Host "[$Stage] reviewing"
  $review = Invoke-Review $Stage $ReviewDependencies ([string]$agent.result)
  if (-not (Test-ReviewWellFormed $review)) {
    $script:ReviewRetryCount++
    $review = Invoke-Review $Stage $ReviewDependencies ([string]$agent.result)
  }
  $programIssues = @(Get-StageProgramIssues $Stage ([string]$agent.result))
  if (-not (Test-ReviewPassed $review) -or $programIssues.Count -gt 0) {
    $script:RepairCount++
    Write-Host "[$Stage] review requested changes; repairing once"
    $programReport = if ($programIssues.Count) { ($programIssues | ForEach-Object { "- $_" }) -join "`n" } else { '无' }
    $repairInput = Convert-ToSafetyContext "$AgentInput`n`n---`n`n# 当前待修订草稿`n$($agent.result)`n`n---`n`n# 独立审查报告`n$($review.result)`n`n---`n`n# 程序硬审问题`n$programReport`n`n请只返回按照审查报告和程序硬审修订后的完整$Stage Markdown。修复全部阻断和一般问题，保持原文边界与项目硬约束，不解释。"
    $agent = Invoke-Agent $Name $repairInput
    $review = Invoke-Review $Stage $ReviewDependencies ([string]$agent.result)
    if (-not (Test-ReviewWellFormed $review)) {
      $script:ReviewRetryCount++
      $review = Invoke-Review $Stage $ReviewDependencies ([string]$agent.result)
    }
  }
  $remainingProgramIssues = @(Get-StageProgramIssues $Stage ([string]$agent.result))
  if ($remainingProgramIssues.Count -gt 0) {
    Write-Host "[$Stage] remaining program issues: $($remainingProgramIssues -join '; ')"
  }
  $reviewText = [string]$review.result
  $reviewScore = [regex]::Match($reviewText, '【总分】\s*(\d{1,3})').Groups[1].Value
  $reviewConclusion = [regex]::Match($reviewText, '【结论】\s*([^\r\n]+)').Groups[1].Value.Trim()
  Write-Host "[$Stage] $reviewConclusion score=$reviewScore"
  if (-not (Test-ReviewPassed $review) -or $remainingProgramIssues.Count -gt 0) {
    $details = if ($remainingProgramIssues.Count) { $remainingProgramIssues -join '; ' } else { "独立审稿未通过：$reviewConclusion score=$reviewScore" }
    throw "$Stage 经过一次定向修订后仍未通过，停止后续阶段：$details"
  }
  return [pscustomobject]@{ Agent = $agent; Review = $review }
}

$projectContext = "# 项目硬约束`n资料类型：小说章节`n视频模型：grok-imagine-video`n画幅：16:9`n分辨率：720p（作为视频任务结构化参数，不依赖 Prompt）`n允许时长：1-15秒`n声音、配音、口型、字幕：暂不制作`n拼接：仅用户点击按钮后执行`n正文边界：严格停在‘下一刻，剧痛骤然归于无尽的死寂。’，不得补写后续觉醒、系统、契约或异界情节"
$directorInput = "$projectContext`n`n---`n`n$sourceMarkdown"
$directorPair = Invoke-StageWithReview 'director' '改编规划' $directorInput "$projectContext`n`n$sourceMarkdown"
$director = $directorPair.Agent
$directorReview = $directorPair.Review
$writerInput = "$projectContext`n`n---`n`n$sourceMarkdown`n`n---`n`n$($director.result)"
$writerPair = Invoke-StageWithReview 'writer' '视频剧本' $writerInput "$projectContext`n`n$sourceMarkdown`n`n$($director.result)"
$writer = $writerPair.Agent
$writerReview = $writerPair.Review
$anchorInput = Convert-ToSafetyContext "$projectContext`n`n---`n`n$sourceMarkdown`n`n---`n`n$($director.result)`n`n---`n`n$($writer.result)"
$anchorPair = Invoke-StageWithReview 'consistency' '视频锚点' $anchorInput "$projectContext`n`n$sourceMarkdown`n`n$($writer.result)"
$anchors = $anchorPair.Agent
$anchorReview = $anchorPair.Review
$storyboardInput = Convert-ToSafetyContext "$projectContext`n`n---`n`n$($director.result)`n`n---`n`n$($writer.result)`n`n---`n`n$($anchors.result)"
$storyboardPair = Invoke-StageWithReview 'storyboard' '视频分镜' $storyboardInput "$projectContext`n`n$sourceMarkdown`n`n$($writer.result)`n`n$($anchors.result)"
$storyboard = $storyboardPair.Agent
$storyboardReview = $storyboardPair.Review
$qcInput = Convert-ToSafetyContext "$projectContext`n`n---`n`n$sourceMarkdown`n`n---`n`n$($director.result)`n`n---`n`n$($writer.result)`n`n---`n`n$($anchors.result)`n`n---`n`n$($storyboard.result)"
$qcPair = Invoke-StageWithReview 'qc' '质检报告复核' $qcInput $qcInput
$qc = $qcPair.Agent
$qcReview = $qcPair.Review

$qcProgramIssues = @(Get-StageProgramIssues '质检报告复核' ([string]$qc.result))
if (-not (Test-QcVerdictPassed ([string]$qc.result)) -or -not (Test-ReviewPassed $qcReview) -or $qcProgramIssues.Count -gt 0) {
  Write-Host '[QC回流] quality gate requested upstream changes; repairing anchors/storyboard once'
  $feedback = "# QC 报告`n$($qc.result)`n`n---`n`n# QC 独立复核`n$($qcReview.result)`n`n---`n`n# QC 程序硬审`n$($qcProgramIssues -join "`n")"
  if ($feedback -match '锚点|state:|固定锚|剧情锚|道具状态') {
    $anchorRepairInput = Convert-ToSafetyContext "$anchorInput`n`n---`n`n# 当前视频锚点`n$($anchors.result)`n`n---`n`n$feedback`n`n请修复所有与锚点有关的问题，返回完整视频锚点 Markdown；保持正确 ID，必要变化要更新变化记录。"
    $anchorPair = Invoke-StageWithReview 'consistency' '视频锚点' $anchorRepairInput "$projectContext`n`n$sourceMarkdown`n`n$($writer.result)"
    $anchors = $anchorPair.Agent
    $anchorReview = $anchorPair.Review
  }
  $storyboardRepairInput = Convert-ToSafetyContext "$projectContext`n`n---`n`n$($director.result)`n`n---`n`n$($writer.result)`n`n---`n`n$($anchors.result)`n`n---`n`n# 当前视频分镜`n$($storyboard.result)`n`n---`n`n$feedback`n`n请修复 QC 指出的全部分镜、锚点引用、原文事实和可执行性问题，返回完整视频分镜 Markdown。不要解释。"
  $storyboardPair = Invoke-StageWithReview 'storyboard' '视频分镜' $storyboardRepairInput "$projectContext`n`n$sourceMarkdown`n`n$($writer.result)`n`n$($anchors.result)"
  $storyboard = $storyboardPair.Agent
  $storyboardReview = $storyboardPair.Review
  $qcInput = Convert-ToSafetyContext "$projectContext`n`n---`n`n$sourceMarkdown`n`n---`n`n$($director.result)`n`n---`n`n$($writer.result)`n`n---`n`n$($anchors.result)`n`n---`n`n$($storyboard.result)"
  $qcPair = Invoke-StageWithReview 'qc' '质检报告复核' $qcInput $qcInput
  $qc = $qcPair.Agent
  $qcReview = $qcPair.Review
}

[IO.Directory]::CreateDirectory($OutputPath) | Out-Null
[IO.File]::WriteAllText((Join-Path $OutputPath '01-source.md'), $sourceMarkdown, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $OutputPath '02-director.md'), [string]$director.result, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $OutputPath '03-script.md'), [string]$writer.result, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $OutputPath '04-anchors.md'), [string]$anchors.result, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $OutputPath '05-storyboard.md'), [string]$storyboard.result, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $OutputPath '06-qc.md'), [string]$qc.result, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $OutputPath '07-director-review.md'), [string]$directorReview.result, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $OutputPath '08-script-review.md'), [string]$writerReview.result, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $OutputPath '09-anchors-review.md'), [string]$anchorReview.result, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $OutputPath '10-storyboard-review.md'), [string]$storyboardReview.result, [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $OutputPath '11-qc-review.md'), [string]$qcReview.result, [Text.UTF8Encoding]::new($false))

$requiredHeadings = @('# 视频分镜', '### 时长', '### 画风锚', '### 场景锚', '### 角色锚', '### 视频 Prompt')
$missing = @($requiredHeadings | Where-Object { -not ([string]$storyboard.result).Contains($_) })
$shots = [regex]::Matches([string]$storyboard.result, '(?m)^##\s+第\s*\d+\s*镜\s*$').Count
$requiredDirectorHeadings = @('# 改编规划', '## 输入边界', '## 项目硬约束', '## 核心视觉母题', '## 风险与自检')
$requiredWriterHeadings = @('# 视频剧本', '## 改编边界', '## 分场剧本', '## 编剧自检')
$requiredAnchorHeadings = @('# 视频锚点', '## 固定锚定', '## 剧情锚点', '### 变化记录')
$requiredQcHeadings = @('【质量评分】', '【原文忠实度】', '【剧情吸引力】', '【视觉独特性】', '【可执行性】', '【硬规则结果】', '【锚点检查】', '【逐镜检查】', '【阻断问题】', '【一般问题】', '【结论】')
$missingDirector = @($requiredDirectorHeadings | Where-Object { -not ([string]$director.result).Contains($_) })
$missingWriter = @($requiredWriterHeadings | Where-Object { -not ([string]$writer.result).Contains($_) })
$missingAnchors = @($requiredAnchorHeadings | Where-Object { -not ([string]$anchors.result).Contains($_) })
$missingQc = @($requiredQcHeadings | Where-Object { -not ([string]$qc.result).Contains($_) })
$forbidden = @('契约达成', '数码核心', '强行接引', '系统提示', '异界觉醒')
$assertiveStory = @([string]$director.result, [string]$writer.result) | ForEach-Object {
  (($_ -split "`r?`n") | Where-Object { $_ -notmatch '不得|禁止|排除|不添加|不出现|未添加|未引入|未包含|未延伸|没有|严格终结|严格停|风险.*无' }) -join "`n"
}
$hallucinations = @($forbidden | Where-Object { $term = $_; $assertiveStory | Where-Object { $_.Contains($term) } })
$wrongAspectCount = [regex]::Matches([string]$storyboard.result, '9\s*:\s*16').Count
$promptBlocks = [regex]::Matches([string]$storyboard.result, '(?ms)^### 视频 Prompt\s*\r?\n(.*?)(?=^##\s+第\s*\d+\s*镜\s*$|\z)')
$missingTargetAspectCount = @($promptBlocks | Where-Object { [regex]::Matches($_.Groups[1].Value, '16\s*:\s*9').Count -ne 1 }).Count
$invalidReferenceCount = 0
$referenceBlocks = [regex]::Matches([string]$storyboard.result, '(?ms)^##\s+第\s*(\d+)\s*镜\s*$.*?(?=^##\s+第\s*\d+\s*镜\s*$|\z)')
foreach ($block in $referenceBlocks) {
  $strategy = [regex]::Match($block.Value, '(?ms)^### 参考方式\s*\r?\n(.*?)\r?\n\r?\n').Groups[1].Value.Trim()
  $referenceAssets = [regex]::Match($block.Value, '(?ms)^### 参考资产\s*\r?\n(.*?)\r?\n\r?\n').Groups[1].Value.Trim()
  if ($strategy -ne 'text' -or $referenceAssets -ne '无') { $invalidReferenceCount++ }
}
$reviews = @($directorReview.result, $writerReview.result, $anchorReview.result, $storyboardReview.result, $qcReview.result)
$malformedReviews = @($reviews | Where-Object { -not ([string]$_).Contains('【结论】') -or -not ([string]$_).Contains('【总分】') -or -not ([string]$_).Contains('【阻断问题】') })
$failedReviews = @($reviews | Where-Object {
  $reviewText = [string]$_
  $conclusion = [regex]::Match($reviewText, '【结论】\s*([^\r\n]+)').Groups[1].Value.Trim()
  $scoreMatch = [regex]::Match($reviewText, '【总分】\s*(\d{1,3})')
  -not $conclusion.StartsWith('通过') -or -not $scoreMatch.Success -or [int]$scoreMatch.Groups[1].Value -lt 80
})
$allOutputs = @($director.result, $writer.result, $anchors.result, $storyboard.result, $qc.result) + $reviews
$providerRefusals = @($allOutputs | Where-Object { [string]$_ -match 'prompt could not be submitted|prohibited use policy|sensitive words|无法提交.{0,20}(敏感|安全策略)|模型.{0,20}安全策略.{0,20}拒绝' })
$productionOutputs = @([string]$director.result, [string]$writer.result, [string]$anchors.result, [string]$storyboard.result)
$assertiveOutputs = @($productionOutputs | ForEach-Object { (($_ -split "`r?`n") | Where-Object { $_ -notmatch '不得|禁止|避免|剔除|不使用|不要复述|已修正' }) -join "`n" })
$graphicProduction = @($assertiveOutputs | Where-Object { $_ -match '骨骼碎裂|血雾|喷血|内脏|残肢|肢解|(身体|肉体|肢体|骨骼).{0,6}碾碎|碾碎.{0,6}(身体|肉体|肢体|骨骼)|(大片|大量).{0,8}(血液|鲜血|猩红).{0,12}(蔓延|流淌|扩散)|染血.{0,6}(面孔|脸|衣物|衣服)' })
$qcOriginalityMatch = [regex]::Match([string]$qc.result, '【视觉独特性】\s*(\d{1,3})')
$inflatedQcScore = $qcOriginalityMatch.Success -and [int]$qcOriginalityMatch.Groups[1].Value -gt 79 -and ([string]$qc.result -match '经典|常见|常规|模板')
$pseudoPhysics = @($assertiveOutputs | Where-Object { $_ -match '反作用力|反向力|(推飞|推开|平推|滑出|飞出).{0,8}(数米|四米|五米)' })
$shotBlocks = [regex]::Matches([string]$storyboard.result, '(?ms)^##\s+第\s*\d+\s*镜\s*$.*?(?=^##\s+第\s*\d+\s*镜\s*$|\z)')
$compoundRescueShots = @($shotBlocks | Where-Object { $_.Value -match '飞扑|鱼跃' -and $_.Value -match '推' -and $_.Value -match '翻滚|跌出|滚跌' -and $_.Value -match '摔倒|扑倒|趴摔|失去平衡' })
[pscustomobject]@{
  BeforeReady = [bool]$before.ready
  AfterReady = [bool]$configured.ready
  Model = [string]$configured.model
  SourceChars = $excerpt.Length
  DirectorChars = ([string]$director.result).Length
  ScriptChars = ([string]$writer.result).Length
  AnchorChars = ([string]$anchors.result).Length
  StoryboardChars = ([string]$storyboard.result).Length
  StoryboardShots = $shots
  MissingHeadings = $missing -join ', '
  MissingDirectorHeadings = $missingDirector -join ', '
  MissingWriterHeadings = $missingWriter -join ', '
  MissingAnchorHeadings = $missingAnchors -join ', '
  MissingQcHeadings = $missingQc -join ', '
  ForbiddenHallucinations = $hallucinations -join ', '
  WrongAspectCount = $wrongAspectCount
  MissingTargetAspectCount = $missingTargetAspectCount
  InvalidReferenceCount = $invalidReferenceCount
  QcChars = ([string]$qc.result).Length
  ReviewCount = $reviews.Count
  RepairCount = $script:RepairCount
  ReviewRetryCount = $script:ReviewRetryCount
  MalformedReviewCount = $malformedReviews.Count
  FailedReviewCount = $failedReviews.Count
  ProviderRefusalCount = $providerRefusals.Count
  GraphicProductionCount = $graphicProduction.Count
  InflatedQcScore = [bool]$inflatedQcScore
  PseudoPhysicsCount = $pseudoPhysics.Count
  CompoundRescueShotCount = $compoundRescueShots.Count
  OutputPath = $OutputPath
} | ConvertTo-Json -Depth 4
if (-not $configured.ready -or $shots -lt 1 -or $missing.Count -gt 0 -or $missingDirector.Count -gt 0 -or $missingWriter.Count -gt 0 -or $missingAnchors.Count -gt 0 -or $missingQc.Count -gt 0 -or $hallucinations.Count -gt 0 -or $wrongAspectCount -gt 0 -or $missingTargetAspectCount -gt 0 -or $invalidReferenceCount -gt 0 -or $malformedReviews.Count -gt 0 -or $failedReviews.Count -gt 0 -or $providerRefusals.Count -gt 0 -or $graphicProduction.Count -gt 0 -or $inflatedQcScore -or $pseudoPhysics.Count -gt 0 -or $compoundRescueShots.Count -gt 0) { exit 1 }
