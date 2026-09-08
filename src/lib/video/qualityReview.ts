import { stripThinking } from "../aiOutput";
import { parseStoryboardShots } from "./storyboard";
import { isVideoProviderRefusal, type VideoMarkdownStage } from "./markdownWorkflow";
import { isPublicHttpsUrl } from "./referenceUrl";

export type VideoQualityReviewStatus = "passed" | "needs_changes";

export interface VideoQualityReviewSummary {
  markdown: string;
  status: VideoQualityReviewStatus;
  score: number | null;
  conclusion: string;
  blockingIssues: string;
  suggestions: string;
}

export interface VideoQcVerdict {
  passed: boolean;
  conclusion: "可生成" | "需修改" | "未知";
  blockingIssues: string;
  generalIssues: string;
  qualityScore: number | null;
  fidelityScore: number | null;
  storyScore: number | null;
  originalityScore: number | null;
  executionScore: number | null;
}

const STAGE_REVIEW_RULES: Record<VideoMarkdownStage, string> = {
  director: `重点核对改编边界、原文事实、项目硬约束和戏剧选择。不得把“更刺激”当成越过原文结尾续写的理由；不得用空洞的爆款术语代替人物动机、因果和具体节拍。`,
  script: `重点核对原文忠实度、人物动机、对白自然度、物理可信度、可拍性和节奏。严查越过原文结尾的续写，识别模板腔、强行煽情、过度形容、无依据超能力、硬造反转和同质化短剧套路。`,
  anchors: `重点核对固定锚定与剧情锚点的边界、跨章继承、稳定 ID、剧情依据和描述完整性。截断、冲突、无依据换装或凭空增加角色/场景/道具都属于阻断问题。`,
  storyboard: `重点核对每镜是否忠实于剧本、单镜是否能独立生成、动作是否能在时长内完成、画幅是否匹配项目、锚点是否真实、状态是否连续。不得因追求冲击力堆叠不可能的动作、运镜或物理交互。`,
  qc: `这是对质检报告本身的复核。检查它是否漏掉越界续写、剧情俗套、人物失真、物理不可信、画幅冲突、锚点冲突、动作超载等问题；不得因为报告写了“可生成”就默认通过。`,
};

export function videoQualityReviewSystem(stage: VideoMarkdownStage): string {
  return `你是独立、严格、对抗性的短剧内容总编与视频生产审核员。你不是生成者的附和者，不重写正文，只审核当前产物。\n\n${STAGE_REVIEW_RULES[stage]}\n\n# 通用标准\n- 原文忠实度：不得补写输入范围之外的后续剧情，不得改变人物关系、世界观和关键结局。\n- 剧情质量：冲突来自人物欲望、选择与因果，不靠口号、形容词、强行反转和“爆款公式”自证精彩。\n- 独特性：区分“制作合规”和“真正精彩”。常见车祸救亲、重生、打脸等母题若没有独有的人物关系细节、选择困境或视觉母题，剧情吸引力与视觉独特性不得轻易高于 79；不得为了提分越过原文添加设定。改进建议也只能重组原文已有细节，不能建议新增原文没有的道具、家庭标识、往事或关键动作。\n- 情感可信度：情绪必须由具体行动与关系积累产生，识别廉价煽情、模板腔和不符合年龄身份的台词。\n- 生产可执行性：符合项目画幅、阶段边界和视频模型能力；声音、配音、口型、字幕、自动拼接当前不进入生产指令。每镜 1-15 秒是单镜限制，不是全片总时长。\n- 评分必须拉开差距。存在越界续写、硬约束冲突、结构残缺、关键锚点冲突或不可执行镜头时，总分不得高于 69。\n- 90 分以上只给少量同时具备忠实、独特、克制、情感可信和高度可执行性的产物。仅格式完整、约束合规且没有明显错误，通常是 75-84 分。\n- “通过”要求总分至少 80，且阻断问题为“无”。不确定时判定“需修改”。\n\n# 唯一输出格式\n只输出以下 Markdown，不要代码围栏：\n# 产物质量审查\n【结论】通过 / 需修改\n【总分】0-100 的整数\n【剧情吸引力】0-100 | 一句话依据\n【视觉独特性】0-100 | 指出独有细节；若只是常见母题必须直说\n【原文忠实度】0-100 | 一句话依据\n【人物与情感】0-100 | 一句话依据\n【可执行性】0-100 | 一句话依据\n【一致性】0-100 | 一句话依据\n【亮点】具体列出真正成立的部分；没有写“无”\n【阻断问题】逐条写“位置 | 问题 | 为什么 | 修正方向”；没有写“无”\n【一般问题】逐条写“位置 | 问题 | 修正方向”；没有写“无”\n【改进建议】按优先级给出可直接作为下一次优化要求的建议。`;
}

export function buildVideoQualityReviewRequest(input: {
  stage: VideoMarkdownStage;
  markdown: string;
  projectContext: string;
  dependencyContext: string;
  deterministicIssues?: string[];
}): string {
  const issues = input.deterministicIssues?.length
    ? input.deterministicIssues.map((issue, index) => `${index + 1}. ${issue}`).join("\n")
    : "无；仍需独立进行语义与内容审核。";
  return `【当前阶段】\n${input.stage}\n\n【项目硬约束】\n${input.projectContext.trim()}\n\n【已保存的原文与前序资料】\n${input.dependencyContext.trim() || "无"}\n\n【程序预检结果】\n${issues}\n\n【待审查产物】\n${input.markdown.trim()}`;
}

function markedValue(markdown: string, marker: string): string {
  const escaped = marker.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = new RegExp(`【${escaped}】\\s*([^\\n]*)`).exec(markdown);
  return match?.[1]?.trim() ?? "";
}

function markedSection(markdown: string, marker: string): string {
  const escaped = marker.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = new RegExp(`【${escaped}】\\s*([\\s\\S]*?)(?=\\n【[^】]+】|$)`).exec(markdown);
  return match?.[1]?.trim() ?? "";
}

function markerCount(markdown: string, marker: string): number {
  const escaped = marker.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return [...markdown.matchAll(new RegExp(`【${escaped}】`, "g"))].length;
}

export function parseVideoQualityReview(raw: string): VideoQualityReviewSummary {
  const cleaned = stripThinking(raw).trim();
  const fenced = /^```(?:markdown|md)?\s*\n([\s\S]*?)\n```$/i.exec(cleaned);
  const markdown = (fenced?.[1] ?? cleaned).trim();
  if (!markdown) throw new Error("质量审查没有返回内容");
  if (isVideoProviderRefusal(markdown)) throw new Error("文本模型拒绝了质量审查请求，拒答内容不会保存为审查报告");
  const required = ["【结论】", "【总分】", "【剧情吸引力】", "【视觉独特性】", "【原文忠实度】", "【人物与情感】", "【可执行性】", "【一致性】", "【亮点】", "【阻断问题】", "【一般问题】", "【改进建议】"];
  const missing = required.filter((marker) => !markdown.includes(marker));
  if (missing.length) throw new Error(`质量审查缺少固定字段：${missing.join("、")}`);
  const duplicate = required.filter((marker) => markerCount(markdown, marker.slice(1, -1)) !== 1);
  if (duplicate.length || markerCount(markdown, "程序阻断") > 1) throw new Error(`质量审查存在重复字段：${duplicate.join("、") || "【程序阻断】"}`);
  const conclusion = markedValue(markdown, "结论");
  const scoreText = markedValue(markdown, "总分");
  const parsedScore = Number(scoreText.match(/\d{1,3}/)?.[0]);
  const score = Number.isFinite(parsedScore) ? Math.max(0, Math.min(100, parsedScore)) : null;
  if (score === null) throw new Error("质量审查总分不是有效的 0-100 整数");
  const programBlocking = markedSection(markdown, "程序阻断");
  const blockingIssues = [markedSection(markdown, "阻断问题"), programBlocking].filter(Boolean).join("\n");
  const suggestions = [markedSection(markdown, "改进建议"), programBlocking ? `必须修复程序阻断：\n${programBlocking}` : ""].filter(Boolean).join("\n\n");
  const explicitlyPassed = /^通过(?:\s|$|\/)/.test(conclusion);
  const noBlockingIssues = /^(无|无。)$/.test(blockingIssues);
  const status: VideoQualityReviewStatus = explicitlyPassed && score >= 80 && noBlockingIssues
    ? "passed"
    : "needs_changes";
  return { markdown, status, score, conclusion, blockingIssues, suggestions };
}

export function deterministicVideoQualityIssues(
  stage: VideoMarkdownStage,
  markdown: string,
  expectedAspectRatio?: string,
  sourceContext = "",
): string[] {
  const issues: string[] = [];
  const assertiveText = markdown.split(/\r?\n/).filter((line) => !/(?:不得|禁止|避免|剔除|不使用|不要复述|已修正)/.test(line)).join("\n");
  if (/骨骼碎裂|血雾|喷血|内脏|残肢|肢解|(?:身体|肉体|肢体|骨骼).{0,6}碾碎|碾碎.{0,6}(?:身体|肉体|肢体|骨骼)|(?:大片|大量).{0,8}(?:血液|鲜血|猩红).{0,12}(?:蔓延|流淌|扩散)|染血.{0,6}(?:面孔|脸|衣物|衣服)/.test(assertiveText)) {
    issues.push("产物包含图形化伤害描述；当前视频生产应改用遮挡、影子、物体反应或画面切黑表达");
  }
  if (/反作用力|反向力|(?:推飞|推开|平推|滑出|飞出).{0,8}(?:数米|四米|五米)/.test(assertiveText)) {
    issues.push("产物使用了可疑的伪物理表述（反作用力滞留或把人物推/滑/飞出数米）；应改成可信的重心失衡、侧向跌离和冲刺惯性，并交由镜头遮挡完成危险结果");
  }
  if (/制动(?:完全)?失灵|刹车失灵/.test(sourceContext)
    && /(?:泥头车|重卡|卡车|车辆).{0,36}(?:锁死|急刹|刹停|停滞|停住|原地停驻|横停)|(?:锁死|急刹|刹停|停滞|停住|原地停驻|横停).{0,36}(?:泥头车|重卡|卡车|车辆)/.test(assertiveText)) {
    issues.push("原始资料明确车辆制动失灵，但产物让车辆锁死急刹或停驻，改变了核心事实与灾难动能");
  }
  if (stage === "anchors") {
    for (const heading of ["# 视频锚点", "## 固定锚定", "## 剧情锚点", "### 变化记录"]) {
      if (!markdown.includes(heading)) issues.push(`锚点文档缺少固定结构：${heading}`);
    }
  }
  if (stage === "storyboard") {
    const shots = parseStoryboardShots(markdown);
    if (!shots.length) return ["视频分镜 Markdown 结构无效或字段不完整"];
    const expected = expectedAspectRatio?.replace(/\s/g, "");
    if (expected === "16:9" || expected === "9:16" || expected === "1:1") {
      const conflicts = shots.filter((shot) => {
        const ratios = shot.videoPrompt.match(/(?:16\s*:\s*9|9\s*:\s*16|1\s*:\s*1)/g)?.map((value) => value.replace(/\s/g, "")) ?? [];
        return ratios.some((ratio) => ratio !== expected);
      });
      if (conflicts.length) issues.push(`第${conflicts.map((shot) => shot.shotNo).join("、")}镜的视频 Prompt 画幅与项目 ${expected} 冲突`);
    }
    const overloaded = shots.filter((shot) => shot.durationS <= 5 && /同时|紧接着|随后|并且|并迅速|继而/.test(`${shot.actionProgress} ${shot.videoPrompt}`));
    if (overloaded.length) issues.push(`第${overloaded.map((shot) => shot.shotNo).join("、")}镜可能在短时长内堆叠多个连续动作，需要人工复核`);
    const compoundRescue = shots.filter((shot) => /(?:飞扑|鱼跃)/.test(`${shot.actionProgress} ${shot.videoPrompt}`)
      && /推/.test(`${shot.actionProgress} ${shot.videoPrompt}`)
      && /(?:翻滚|跌出|滚跌)/.test(`${shot.actionProgress} ${shot.videoPrompt}`)
      && /(?:摔倒|扑倒|趴摔|失去平衡)/.test(`${shot.actionProgress} ${shot.videoPrompt}`));
    if (compoundRescue.length) issues.push(`第${compoundRescue.map((shot) => shot.shotNo).join("、")}镜同时承担接近、推人和双方位移结果，属于不可控复合救援镜头，应拆成独立镜头`);
  }
  if (stage === "qc") {
    for (const marker of ["【质量评分】", "【原文忠实度】", "【剧情吸引力】", "【视觉独特性】", "【可执行性】", "【硬规则结果】", "【锚点检查】", "【逐镜检查】", "【阻断问题】", "【一般问题】", "【结论】"]) {
      if (!markdown.includes(marker)) issues.push(`质检报告缺少字段：${marker}`);
    }
    if (markdown.length < 500) issues.push("质检报告过短，无法证明已完成逐镜、锚点、原文忠实度和项目硬规则检查");
    const originality = Number(markedValue(markdown, "视觉独特性").match(/\d{1,3}/)?.[0]);
    if (Number.isFinite(originality) && originality > 79 && /经典|常见|常规|模板/.test(markdown)) {
      issues.push("质检承认内容属于经典/常见/模板母题，却把视觉独特性评为 80 分以上，评分校准不可信");
    }
  }
  return issues;
}

export function parseVideoQcVerdict(markdown: string): VideoQcVerdict {
  const required = ["质量评分", "原文忠实度", "剧情吸引力", "视觉独特性", "可执行性", "阻断问题", "一般问题", "结论"];
  if (required.some((marker) => markerCount(markdown, marker) !== 1)) {
    return { passed: false, conclusion: "未知", blockingIssues: "质检报告字段缺失或重复", generalIssues: "", qualityScore: null, fidelityScore: null, storyScore: null, originalityScore: null, executionScore: null };
  }
  const blockingIssues = markedSection(markdown, "阻断问题");
  const generalIssues = markedSection(markdown, "一般问题");
  const rawConclusion = markedValue(markdown, "结论");
  const conclusion: VideoQcVerdict["conclusion"] = /^可生成(?:\s|$)/.test(rawConclusion)
    ? "可生成"
    : /^需修改(?:\s|$)/.test(rawConclusion)
      ? "需修改"
      : "未知";
  const noBlockers = /^(无|无。)$/.test(blockingIssues);
  const score = (marker: string) => {
    const value = Number(markedValue(markdown, marker).match(/\d{1,3}/)?.[0]);
    return Number.isFinite(value) && value >= 0 && value <= 100 ? value : null;
  };
  const qualityScore = score("质量评分");
  const fidelityScore = score("原文忠实度");
  const storyScore = score("剧情吸引力");
  const originalityScore = score("视觉独特性");
  const executionScore = score("可执行性");
  return {
    passed: conclusion === "可生成" && noBlockers
      && qualityScore !== null && qualityScore >= 75
      && fidelityScore !== null && fidelityScore >= 75
      && storyScore !== null && storyScore >= 65
      && executionScore !== null && executionScore >= 75,
    conclusion,
    blockingIssues,
    generalIssues,
    qualityScore,
    fidelityScore,
    storyScore,
    originalityScore,
    executionScore,
  };
}

function anchorIds(markdown: string): Set<string> {
  return new Set([...markdown.matchAll(/^(?:(?:style|character|scene|prop):|state:(?:character|scene|prop):)[^\s|]+/gm)].map((match) => match[0]));
}

export function storyboardProductionIssues(input: {
  storyboardMarkdown: string;
  anchorMarkdown: string;
  expectedAspectRatio?: string;
  availableAssetIds?: Iterable<string>;
  availableAssets?: Iterable<{ id: string; kind: string; path: string }>;
  sourceContext?: string;
}): string[] {
  const issues = deterministicVideoQualityIssues("storyboard", input.storyboardMarkdown, input.expectedAspectRatio, input.sourceContext);
  const shots = parseStoryboardShots(input.storyboardMarkdown);
  if (!shots.length) return issues;
  const knownAnchors = anchorIds(input.anchorMarkdown);
  const anchorDescriptions = new Map([...input.anchorMarkdown.matchAll(/^(\S+)\s*\|\s*(.+)$/gm)].map((match) => [match[1], match[2]]));
  for (const shot of shots) {
    const referenced = [shot.styleAnchor, ...shot.sceneAnchor.split(/[,，、\n]/), ...shot.characterAnchors, ...shot.propAnchors].map((id) => id.trim()).filter(Boolean);
    const missing = referenced.filter((id) => !knownAnchors.has(id));
    if (missing.length) issues.push(`第${shot.shotNo}镜引用了不存在的锚点：${missing.join("、")}`);
  }
  for (const shot of shots) {
    const shotText = `${shot.shotType} ${shot.camera} ${shot.action} ${shot.startState} ${shot.actionProgress} ${shot.endState} ${shot.videoPrompt}`;
    if (!/(?:主观|第一人称|POV)/i.test(shotText)) continue;
    const owner = /([\p{Script=Han}A-Za-z0-9_]{1,12})(?:的)?(?:纯)?(?:主观|第一人称)/u.exec(shotText)?.[1];
    if (!owner) continue;
    const conflicting = shot.characterAnchors.filter((id) => id.includes(owner) && /面部|面容|嘴角|五官|脸部|脸型|神情/.test(anchorDescriptions.get(id) ?? ""));
    if (conflicting.length) issues.push(`第${shot.shotNo}镜是${owner}的第一人称主观镜头，却绑定了描述其自身面部的锚点：${conflicting.join("、")}`);
  }
  const assetRecords = new Map([...(input.availableAssets ?? [])].map((asset) => [asset.id, asset]));
  const availableAssets = new Set([...(input.availableAssetIds ?? []), ...assetRecords.keys()]);
  for (const shot of shots) {
    if (shot.referenceStrategy === "text") continue;
    if (!shot.referenceAssetIds.length) {
      issues.push(`第${shot.shotNo}镜使用 ${shot.referenceStrategy}，但没有参考资产`);
      continue;
    }
    const missing = shot.referenceAssetIds.filter((id) => !availableAssets.has(id));
    if (missing.length) issues.push(`第${shot.shotNo}镜的参考资产不存在：${missing.join("、")}`);
    const anchorsUsedAsAssets = shot.referenceAssetIds.filter((id) => /^(?:style|character|scene|prop|state):/.test(id));
    if (anchorsUsedAsAssets.length) issues.push(`第${shot.shotNo}镜把文本锚点误当成媒体参考资产：${anchorsUsedAsAssets.join("、")}`);
    const records = shot.referenceAssetIds.map((id) => assetRecords.get(id)).filter((asset): asset is { id: string; kind: string; path: string } => Boolean(asset));
    const unsupported = records.filter((asset) => asset.kind !== "image" && asset.kind !== "video");
    if (unsupported.length) issues.push(`第${shot.shotNo}镜引用了非图片/视频资产：${unsupported.map((asset) => asset.id).join("、")}`);
    const nonPublicVideos = records.filter((asset) => asset.kind === "video" && !isPublicHttpsUrl(asset.path));
    if (nonPublicVideos.length) issues.push(`第${shot.shotNo}镜的参考视频必须是无凭据的公网 HTTPS URL：${nonPublicVideos.map((asset) => asset.id).join("、")}`);
    const nonPublicImageUrls = records.filter((asset) => asset.kind === "image"
      && /^[a-z][a-z\d+.-]*:/i.test(asset.path)
      && !/^[a-z]:[\\/]/i.test(asset.path)
      && !isPublicHttpsUrl(asset.path));
    if (nonPublicImageUrls.length) issues.push(`第${shot.shotNo}镜的图片 URL 必须是无凭据的公网 HTTPS URL：${nonPublicImageUrls.map((asset) => asset.id).join("、")}`);
    if (shot.referenceStrategy === "first_frame" && (records.length !== 1 || records[0]?.kind !== "image")) issues.push(`第${shot.shotNo}镜的 first_frame 必须且只能引用 1 张图片资产`);
  }
  return [...new Set(issues)];
}
