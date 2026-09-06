import { DRAMA_DOCS, type DramaDocName } from "./docs";
import type { LibAsset } from "../../store/useLibraryStore";
import { getDocumentMeta } from "../documents";

export type DramaStageId =
  | "drama_position"
  | "drama_world"
  | "drama_engine"
  | "drama_beats"
  | "drama_draft"
  | "drama_review";

export interface DramaStage {
  id: DramaStageId;
  no: number;
  label: string;
  agentLabel: string;
  artifactTitle: string;
  extraArtifacts?: string[];
  docs: DramaDocName[];
  readArtifacts: string[];
  placeholder: string;
  system: string;
}

export const DRAMA_ARTIFACTS = {
  anchor: "项目锚定卡.md",
  world: "世界与角色设定集.md",
  beats: "5分钟卡点大纲.md",
  draft: "5分钟短剧-剧本初稿.md",
  review: "剧本审查报告.md",
  final: "5分钟短剧-剧本定稿.md",
  storyboard: "5分钟短剧-分镜脚本.md",
} as const;

const SHARED_RULES = `你是五分钟短剧大赛编剧顾问，不是连载短剧编剧。

# 身份
站在大赛初审评委和第一次观看的观众位置工作。用户可能只有一句自然语言，例如「帮我写一个五分钟短剧大赛剧本」。

# 绝对禁令
- 禁止跳步：只完成本阶段交付物，不得提前写后一阶段正文。
- 禁止连载体：无「第X集」、无 500–750 字/集、无季/集大纲。
- 禁止凭记忆编造赛道、原型、格式；只能用本轮提供的参考文档和已落盘文件。
- 禁止中途更换已锁定的主结构原型，除非用户明确要求并确认。
- 禁止在审查通过并定稿之前提出或生成分镜。
- 禁止用设定集、口头解释或作者意图替剧本补分。
- 禁止一次抛多个互不相关的问题。每轮结尾只能有一个【待确认】。

# 每轮结构
1. 先给本阶段可用的完整交付正文（用下方规定标记）。
2. 若关键字段用户没锁死，先给 2–4 个选项，不要假装已经锁定。
3. 最后只问一件事，格式必须是：
【待确认】一个问题或一组选项

# 篇幅与形态
- 全片约 5 分钟，画面 20–40 个，正文约 6000 字（5500–6500），仅阶段5/6才写到这个篇幅。
- 30 秒是入场券，后 4.5 分钟必须写完并兑现开场承诺。
- 横屏/竖屏不限；默认视觉形态为 AI 真人剧，除非用户另指定。

# 文风硬禁（违反则本阶段未完成）
- 禁止网文/短视频腔：不许「开局」「废柴」「逆袭」「极道」当片名，不许问号感叹号标题。
- 禁止空转形容词：太古、毁天灭地、万载、九天十地、至尊、神级、宛如、如同、轰然、狂暴。每场最多 1 个强度词。
- 禁止说明书：不解释法则、穿越、评级；立意不说破。
- 改编小说只抽一条可拍的线，禁止章节缩写。
- 落盘正文禁止写入【待确认】；【待确认】只出现在对话末尾，不进文件。`;

export const DRAMA_STAGES: DramaStage[] = [
  {
    id: "drama_position",
    no: 1,
    label: "项目定位与创作路线",
    agentLabel: "定位",
    artifactTitle: DRAMA_ARTIFACTS.anchor,
    docs: ["tracks.md", "archetypes.md", "centers.md", "craft.md"],
    readArtifacts: [],
    placeholder: "例如：帮我写一个五分钟短剧大赛剧本 / 古风赛道，角色向",
    system: `${SHARED_RULES}

# 本阶段任务
只做项目定位。锁赛道、一句话故事、核心立意、主结构原型、主叙事重心、视觉呈现形态。
用户没想好赛道时，先用 tracks.md 的五大赛道给选项，不要开写世界设定或剧本。

# 必读
tracks.md、archetypes.md、centers.md。原型必须从 12 个高命中结构里选，并写清启动—升级—假高潮—结尾。

# 输出（落盘 项目锚定卡.md，必须用这些标记）
【赛道】赛道 / 子类 / 为何适合 5 分钟
【一句话故事】谁在什么压力下必须做什么，否则会怎样
【核心立意】用动作兑现的主题，禁止写成角色台词
【主结构原型】名称 + 启动 / 升级 / 假高潮 / 结尾兑现
【主叙事重心】角色向 / 创意向 / 强剧情向，三选一及理由
【视觉形态】默认 AI 真人剧；画幅待定或按用户指定
【待确认】只问一件事`,
  },
  {
    id: "drama_world",
    no: 2,
    label: "世界观与角色设定",
    agentLabel: "设定",
    artifactTitle: DRAMA_ARTIFACTS.world,
    docs: ["tracks.md", "archetypes.md", "craft.md"],
    readArtifacts: [DRAMA_ARTIFACTS.anchor],
    placeholder: "确认世界法则、铁三角角色，或指出要改的设定",
    system: `${SHARED_RULES}

# 本阶段任务
为已锁定原型建立支点。只写设定，不写分场，不写四幕，不改原型。
铁三角 ≤3 个核心势力：主角、施压方、关键配角。

# 必读
已落盘 项目锚定卡.md，以及 tracks.md、archetypes.md。
写每个角色和场景时点名调用锚定卡里的赛道、原型、一句话故事。

# 输出（落盘 世界与角色设定集.md）
【世界法则】可拍的规则；谁有权、什么不能做
【关键场景】2–5 个可反复出现的地点，含时间/光线/标志物
【关键道具】能被看见、被夺走、被当证据的物件
【铁三角·主角】欲望、阻碍、把柄、不能退的理由、外形锚点
【铁三角·施压方】动机自洽、为何施压、把柄、外形锚点
【铁三角·关键配角】与主角的关系支点、外形锚点
【原型支点】这个原型如何在本世界里启动
【一致性禁忌】后续严禁违反的清单
【待确认】只问一件事`,
  },
  {
    id: "drama_engine",
    no: 3,
    label: "戏剧引擎与首尾双锁",
    agentLabel: "引擎",
    artifactTitle: DRAMA_ARTIFACTS.anchor,
    extraArtifacts: [DRAMA_ARTIFACTS.anchor],
    docs: ["centers.md", "archetypes.md", "craft.md"],
    readArtifacts: [DRAMA_ARTIFACTS.anchor, DRAMA_ARTIFACTS.world],
    placeholder: "确认 30 秒开场和结尾兑现，或指出开场不够入戏的地方",
    system: `${SHARED_RULES}

# 本阶段任务
一次锁定 30 秒开场和结尾兑现。更新项目锚定卡，不要写完整四幕或对白。
开场必须让第一次看的观众在 30 秒内看懂：这是谁、处境是什么、异常点在哪。

# 必读
已落盘 项目锚定卡.md、世界与角色设定集.md，以及 centers.md、archetypes.md。
30 秒按 3-10-30：3 秒看见人与异常，10 秒看懂处境，30 秒压力成立。

# 输出（覆盖更新 项目锚定卡.md，保留阶段1全部字段）
【赛道】沿用
【一句话故事】沿用或微调，不得换故事
【核心立意】沿用
【主结构原型】沿用
【主叙事重心】沿用或在用户确认下锁死
【视觉形态】沿用
【30秒开场】按 3 秒 / 10 秒 / 30 秒写可拍画面，禁止旁白解释设定
【结尾兑现】如何回收开场细节、不可逆选择、余味从哪来
【原型执行表】启动 / 升级 / 假高潮 / 结尾，对应本故事的具体动作
【一致性禁忌】合并设定集禁忌，后续严禁违反
【待确认】只问一件事`,
  },
  {
    id: "drama_beats",
    no: 4,
    label: "四幕因果推进与极限卡点",
    agentLabel: "大纲",
    artifactTitle: DRAMA_ARTIFACTS.beats,
    docs: ["beats-5min.md", "centers.md", "craft.md"],
    readArtifacts: [DRAMA_ARTIFACTS.anchor, DRAMA_ARTIFACTS.world],
    placeholder: "确认四幕卡点，或指出哪一幕没有新后果",
    system: `${SHARED_RULES}

# 本阶段任务
把故事卡进 5 分钟生理节奏。每一步必须有新信息或新后果，禁止灌水过场。
必须映射已锁原型：幕一启动、幕二升级、幕三假高潮、幕四结尾兑现。

# 必读
已落盘 项目锚定卡.md、世界与角色设定集.md，以及 beats-5min.md、centers.md。
时间：幕一 0:00–0:30，幕二 0:30–2:00，幕三 2:00–3:30，幕四 3:30–5:00。

# 输出（落盘 5分钟卡点大纲.md）
【卡点大纲】
用表格行：时间码 | 场景 | 新信息或新后果 | 人物选择 | 视觉钩子
四幕都要写满。后 4.5 分钟必须兑现开场承诺。
【节奏自检】指出若删掉哪一步信息/后果会塌，禁止空话。
【待确认】只问一件事`,
  },
  {
    id: "drama_draft",
    no: 5,
    label: "分段剧本落地",
    agentLabel: "初稿",
    artifactTitle: DRAMA_ARTIFACTS.draft,
    docs: ["format-formal.md", "beats-5min.md", "craft.md"],
    readArtifacts: [DRAMA_ARTIFACTS.anchor, DRAMA_ARTIFACTS.world, DRAMA_ARTIFACTS.beats, DRAMA_ARTIFACTS.draft],
    placeholder: "确认本幕后再写下幕；可说「先写第一幕」或指出对白太解释",
    system: `${SHARED_RULES}

# 本阶段任务
按四幕一幕一幕写正式剧本。默认本轮只写用户指定的那一幕；用户没指定则只写第一幕并停下确认。
  写画面与对白必须点名调用设定集里的场景、道具、禁忌、角色锚点，严禁临时发明新设定。
  已落盘初稿里的幕次默认保留；本轮修订某幕时重写该幕，不要把已完成的其他幕删掉。
  每写完一幕做对白去解释化自检：删掉解释设定、复述剧情、说出立意的台词。

# 必读
已落盘 项目锚定卡.md、世界与角色设定集.md、5分钟卡点大纲.md，以及 format-formal.md。
正式格式：文首元数据；每场「内/外 地点 时间」；画面写可拍动作；对白写潜台词。

# 输出（落盘 5分钟短剧-剧本初稿.md）
【元数据】片名、赛道、重心、原型、时长、画幅、立意、一句话故事
【本幕正文】只含本轮完成的幕次；未完成则标题写「第X幕，待续」
【对白自检】列出删掉或改写的解释性台词
【待确认】只问要不要写下幕，或指出本幕唯一要改的一点
禁止分镜。全片目标 20–40 个画面、约 6000 字，但本轮不得为凑字数一次写完四幕，除非用户明确说「四幕一次性写完」。
每场画面 80–180 字。禁止把【待确认】写进落盘正文。`,
  },
  {
    id: "drama_review",
    no: 6,
    label: "剧本审查与定稿",
    agentLabel: "审查",
    artifactTitle: DRAMA_ARTIFACTS.review,
    extraArtifacts: [DRAMA_ARTIFACTS.final],
    docs: ["review-standards.md", "format-formal.md", "craft.md"],
    readArtifacts: [DRAMA_ARTIFACTS.anchor, DRAMA_ARTIFACTS.world, DRAMA_ARTIFACTS.beats, DRAMA_ARTIFACTS.draft],
    placeholder: "让审查按三条硬指标判定；通过后才能定稿，定稿后才可做分镜",
    system: `${SHARED_RULES}

# 本阶段任务
以大赛初审评委视角审已落盘初稿。第一次看、没有任何前情说明。
不许用设定集或用户口头解释替剧本补分。先审查，后定稿，再分镜。

# 三条硬指标（每条必须出现「通过」或「待修」或「重写」之一）
1. 开篇是否清晰：观众 30 秒内能否不费力看懂这是谁、处境是什么、异常点在哪。
2. 冲突来源是否可信：施压方动机是否自洽，主角为什么不能退，代价是否被演过，压力是否逐幕递增。
3. 结尾钩子是否有余味：是否兑现开场承诺并回收开场细节，主角是否做出不可逆选择，立意是否不靠台词说破。

# 处置
- 待修：给修改清单，指定幕次，等用户改完再复审，本轮不要输出定稿。
- 重写：开篇回阶段3，冲突回阶段2或4，结尾回阶段3或4；本轮不要输出定稿。
- 三条均为通过：才追加完整定稿。定稿必须从第一场写到最后一场，禁止截断。
- 网文片名、说明书台词、特效灌水、画面不足 20、结尾没回收开场细节：对应项不得「通过」。修改方向禁止写「无」。

# 输出（先落盘 剧本审查报告.md）
【开篇】通过/待修/重写
证据：（引用初稿原文）
修改方向：（可执行）
【冲突】通过/待修/重写
证据：
修改方向：
【结尾】通过/待修/重写
证据：
修改方向：
【总评】是否允许定稿
仅当三条都是通过时，再输出：
# 定稿
（完整正式剧本）
禁止分镜。`,
  },
];

export function dramaStage(id: string): DramaStage | undefined {
  return DRAMA_STAGES.find((item) => item.id === id);
}

export function injectDramaDocs(stage: DramaStage): string {
  return injectNamedDocs(stage.docs);
}

export function injectNamedDocs(names: DramaDocName[]): string {
  return names.map((name) => `【参考文档 ${name}】\n${DRAMA_DOCS[name]}`).join("\n\n");
}

export function artifactText(asset?: LibAsset): string {
  if (!asset) return "";
  return getDocumentMeta(asset)?.text || String(asset.params?.text ?? "");
}

export function latestArtifact(assets: LibAsset[], projectId: string | null, title: string): LibAsset | undefined {
  // 严格项目隔离：LibAsset.projectId 为 string | undefined，归一成 null 后严格相等比较；
  // projectId 为 null 时只匹配无项目文档，绝不跨项目泄漏。
  return assets
    .filter((asset) => asset.asset.kind === "text" && (asset.projectId ?? null) === projectId && (asset.source === title || getDocumentMeta(asset)?.title === title))
    .sort((a, b) => b.createdAt - a.createdAt)[0];
}

const STAGE_MARKERS: Partial<Record<DramaStageId, string[]>> = {
  drama_position: ["【赛道】", "【主结构原型】", "【一句话故事】"],
  drama_world: ["【世界法则】", "【铁三角·主角】", "【一致性禁忌】"],
  drama_engine: ["【30秒开场】", "【结尾兑现】", "【原型执行表】"],
  drama_beats: ["【卡点大纲】"],
};

const STAGE_DRAFT_MARKERS: Record<"drama_draft", string[]> = {
  drama_draft: ["第1幕", "第2幕", "第3幕", "第4幕"],
};

const REVIEW_MARKERS = ["【开篇】通过", "【冲突】通过", "【结尾】通过", "# 定稿"];

export interface DramaStageAssessment {
  stage: DramaStage;
  complete: boolean;
  partial: boolean;
  missing: string[];
}

export interface DramaStageBundleAssessment {
  assessments: DramaStageAssessment[];
  completeStageIds: DramaStageId[];
  partialStageIds: DramaStageId[];
}

export interface DramaStagePersistencePlan extends DramaStageBundleAssessment {
  adoptedStageIds: DramaStageId[];
  blockedCompleteStageIds: DramaStageId[];
}

/** A separate title prevents an incomplete reply from replacing a usable stage artifact. */
export function stagePartialArtifactTitle(stage: DramaStage): string {
  return `五分钟短剧·阶段${stage.no}草稿·${stage.label}.md`;
}

const ACT_SPECS = [
  { n: 1 as const, re: /(?:^|\n)\s*(?:第\s*1\s*幕|第一幕|幕一)/ },
  { n: 2 as const, re: /(?:^|\n)\s*(?:第\s*2\s*幕|第二幕|幕二)/ },
  { n: 3 as const, re: /(?:^|\n)\s*(?:第\s*3\s*幕|第三幕|幕三)/ },
  { n: 4 as const, re: /(?:^|\n)\s*(?:第\s*4\s*幕|第四幕|幕四)/ },
];

export type ReviewVerdict = "通过" | "待修" | "重写" | "未知";

export function hasMarkers(text: string, markers: string[]): boolean {
  return markers.every((marker) => text.includes(marker));
}

export function stageRequiredMarkers(stage: DramaStage): string[] {
  if (stage.id === "drama_draft") return STAGE_DRAFT_MARKERS.drama_draft;
  if (stage.id === "drama_review") return REVIEW_MARKERS;
  return STAGE_MARKERS[stage.id] ?? [];
}

export function presentActs(text: string): number[] {
  return ACT_SPECS.filter((spec) => spec.re.test(text)).map((spec) => spec.n);
}

export function draftComplete(text: string): boolean {
  return presentActs(text).length === 4;
}

export function nextMissingAct(text: string): 1 | 2 | 3 | 4 {
  const have = new Set(presentActs(text));
  return ([1, 2, 3, 4] as const).find((n) => !have.has(n)) ?? 4;
}

export interface DraftParts {
  preamble: string;
  acts: Partial<Record<1 | 2 | 3 | 4, string>>;
  tail: string;
}

function actHeaderPositions(text: string): Array<{ n: 1 | 2 | 3 | 4; index: number }> {
  const found: Array<{ n: 1 | 2 | 3 | 4; index: number }> = [];
  for (const spec of ACT_SPECS) {
    const match = spec.re.exec(text);
    if (match && match.index != null) found.push({ n: spec.n, index: match.index });
  }
  return found.sort((a, b) => a.index - b.index);
}

export function splitDraft(text: string): DraftParts {
  const positions = actHeaderPositions(text);
  const tailMatch = /【对白自检】|【待确认】/.exec(text);
  const lastAct = positions[positions.length - 1];
  const tailIndex = tailMatch && (positions.length === 0 || (tailMatch.index ?? 0) > (lastAct?.index ?? 0))
    ? tailMatch.index!
    : text.length;
  const preamble = positions.length ? text.slice(0, positions[0].index) : text.slice(0, tailIndex);
  const acts: DraftParts["acts"] = {};
  positions.forEach((pos, index) => {
    const end = index + 1 < positions.length ? positions[index + 1].index : tailIndex;
    acts[pos.n] = text.slice(pos.index, end);
  });
  return { preamble, acts, tail: text.slice(tailIndex) };
}

export function mergeDraft(existing: string, incoming: string): string {
  if (!existing.trim()) return incoming;
  if (!incoming.trim()) return existing;
  if (draftComplete(incoming)) return incoming;
  const nextActs = presentActs(incoming);
  if (!nextActs.length) return `${existing.trim()}\n\n${incoming.trim()}`;
  const old = splitDraft(existing);
  const next = splitDraft(incoming);
  const acts = { ...old.acts, ...next.acts };
  const chunks: string[] = [];
  const preamble = next.preamble.trim() || old.preamble;
  if (preamble.trim()) chunks.push(preamble.trim());
  for (const n of [1, 2, 3, 4] as const) {
    const body = acts[n];
    if (body?.trim()) chunks.push(body.trim());
  }
  const tail = next.tail.trim();
  if (tail) chunks.push(tail);
  return chunks.join("\n\n");
}

export function extractFinalScript(text: string): string {
  const index = text.indexOf("# 定稿");
  if (index < 0) return "";
  return stripDeliveryMeta(text.slice(index).replace(/^# 定稿\s*/, "").trim());
}

export function stripDeliveryMeta(text: string): string {
  return text
    .replace(/\n*【待确认】[\s\S]*$/u, "")
    .replace(/\n*【对白自检】[\s\S]*$/u, "")
    .replace(/\n*---+[\s\S]*【待确认】[\s\S]*$/u, "")
    .trim();
}

export function persistableArtifactText(stage: DramaStage, incoming: string): string {
  const cleaned = stripDeliveryMeta(incoming);
  if (stage.id === "drama_review") {
    const review = stripDeliveryMeta(incoming.replace(/\n*# 定稿[\s\S]*$/u, "").trim());
    const finalBody = extractFinalScript(incoming);
    return finalBody ? `${review}\n\n# 定稿\n${finalBody}` : review;
  }
  return cleaned;
}

export function storyboardShotCount(text: string): number {
  return [...text.matchAll(/^\s*\|\s*\d+\s*\|/gm)].length;
}

export function looksTruncated(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed) return true;
  return /[，、：:；;（(「“]$/.test(trimmed) || /连一秒$/.test(trimmed);
}

export function parseReviewVerdicts(text: string): Array<{ label: string; verdict: ReviewVerdict }> {
  return ["开篇", "冲突", "结尾"].map((label) => {
    const match = text.match(new RegExp(`【${label}】\\s*(通过|待修|重写)`));
    return { label, verdict: (match?.[1] as ReviewVerdict | undefined) ?? "未知" };
  });
}

export function reviewPassed(text: string): boolean {
  return parseReviewVerdicts(text).every((item) => item.verdict === "通过");
}

export function assessDramaStage(stage: DramaStage, incoming: string): DramaStageAssessment {
  const text = incoming.trim();
  if (!text) return { stage, complete: false, partial: false, missing: stageRequiredMarkers(stage) };

  if (stage.id === "drama_draft") {
    const acts = new Set(presentActs(text));
    const missing = ([1, 2, 3, 4] as const)
      .filter((act) => !acts.has(act))
      .map((act) => `第${act}幕`);
    return { stage, complete: missing.length === 0, partial: acts.size > 0 && missing.length > 0, missing };
  }

  if (stage.id === "drama_review") {
    const verdicts = parseReviewVerdicts(text);
    const missing = verdicts
      .filter((item) => item.verdict !== "通过")
      .map((item) => `【${item.label}】通过${item.verdict === "未知" ? "" : `（当前：${item.verdict}）`}`);
    if (!extractFinalScript(text)) missing.push("# 定稿");
    const hasReviewEvidence = verdicts.some((item) => item.verdict !== "未知") || text.includes("# 定稿");
    return { stage, complete: missing.length === 0, partial: hasReviewEvidence && missing.length > 0, missing };
  }

  const required = stageRequiredMarkers(stage);
  const missing = required.filter((marker) => !text.includes(marker));
  return {
    stage,
    complete: required.length > 0 && missing.length === 0,
    partial: required.some((marker) => text.includes(marker)) && missing.length > 0,
    missing,
  };
}

/**
 * Recognition deliberately reuses the existing completion gates. It does not
 * infer a stage from free prose: a later stage is eligible only with all of
 * its required markers, while a partial is retained separately for recovery.
 */
export function assessDramaStageBundle(incoming: string, firstStageId: DramaStageId): DramaStageBundleAssessment {
  const start = Math.max(0, DRAMA_STAGES.findIndex((stage) => stage.id === firstStageId));
  const assessments = DRAMA_STAGES.slice(start).map((stage) => assessDramaStage(stage, incoming));
  return {
    assessments,
    completeStageIds: assessments.filter((item) => item.complete).map((item) => item.stage.id),
    partialStageIds: assessments.filter((item) => item.partial).map((item) => item.stage.id),
  };
}

/**
 * Later complete-looking blocks are not adopted across an incomplete gate.
 * This retains the six-stage confirmation order while making a fully valid
 * bundled response recoverable in one submission.
 */
export function planDramaStagePersistence(incoming: string, firstStageId: DramaStageId): DramaStagePersistencePlan {
  const bundle = assessDramaStageBundle(incoming, firstStageId);
  const adoptedStageIds: DramaStageId[] = [];
  const blockedCompleteStageIds: DramaStageId[] = [];
  let blocked = false;
  for (const assessment of bundle.assessments) {
    if (!blocked && assessment.complete) {
      adoptedStageIds.push(assessment.stage.id);
      continue;
    }
    blocked = true;
    if (assessment.complete) blockedCompleteStageIds.push(assessment.stage.id);
  }
  return { ...bundle, adoptedStageIds, blockedCompleteStageIds };
}

export function shouldPersistArtifact(stage: DramaStage, incoming: string): boolean {
  return assessDramaStage(stage, incoming).complete;
}

export function shouldPersistPartialArtifact(stage: DramaStage, incoming: string): boolean {
  return assessDramaStage(stage, incoming).partial;
}

export function reviewRewindStage(text: string): DramaStageId | null {
  const verdicts = parseReviewVerdicts(text);
  const of = (label: string) => verdicts.find((item) => item.label === label)?.verdict;
  if (of("开篇") === "重写") return "drama_engine";
  if (of("冲突") === "重写") return "drama_world";
  if (of("结尾") === "重写") return "drama_engine";
  return null;
}

export function stageComplete(assets: LibAsset[], projectId: string | null, stage: DramaStage): boolean {
  const artifact = latestArtifact(assets, projectId, stage.artifactTitle);
  if (!artifact) return false;
  const text = artifactText(artifact);
  const markers = STAGE_MARKERS[stage.id];
  if (markers) return hasMarkers(text, markers);
  if (stage.id === "drama_draft") return draftComplete(text);
  if (stage.id === "drama_review") {
    return reviewPassed(text) && Boolean(latestArtifact(assets, projectId, DRAMA_ARTIFACTS.final));
  }
  return Boolean(text.trim());
}

export function stagePartial(assets: LibAsset[], projectId: string | null, stage: DramaStage): DramaStageAssessment | null {
  if (stageComplete(assets, projectId, stage)) return null;
  const artifact = latestArtifact(assets, projectId, stagePartialArtifactTitle(stage));
  if (!artifact) return null;
  const assessment = assessDramaStage(stage, artifactText(artifact));
  return assessment.partial ? assessment : null;
}

export function stageUnlocked(assets: LibAsset[], projectId: string | null, stage: DramaStage): boolean {
  if (stage.no === 1) return true;
  return DRAMA_STAGES.filter((item) => item.no < stage.no).every((item) => stageComplete(assets, projectId, item));
}

export function canStoryboard(assets: LibAsset[], projectId: string | null): boolean {
  const review = latestArtifact(assets, projectId, DRAMA_ARTIFACTS.review);
  return Boolean(latestArtifact(assets, projectId, DRAMA_ARTIFACTS.final)) && Boolean(review && reviewPassed(artifactText(review)));
}

export function firstOpenStage(assets: LibAsset[], projectId: string | null): DramaStage {
  return DRAMA_STAGES.find((item) => stageUnlocked(assets, projectId, item) && !stageComplete(assets, projectId, item))
    ?? DRAMA_STAGES[DRAMA_STAGES.length - 1];
}

export const ARTIFACT_SLOTS = [
  { title: DRAMA_ARTIFACTS.anchor, hint: "阶段1定位 / 阶段3引擎" },
  { title: DRAMA_ARTIFACTS.world, hint: "阶段2设定" },
  { title: DRAMA_ARTIFACTS.beats, hint: "阶段4大纲" },
  { title: DRAMA_ARTIFACTS.draft, hint: "阶段5初稿（四幕合并）" },
  { title: DRAMA_ARTIFACTS.review, hint: "阶段6审查" },
  { title: DRAMA_ARTIFACTS.final, hint: "三条通过后定稿" },
  { title: DRAMA_ARTIFACTS.storyboard, hint: "定稿后分镜" },
] as const;

export const STORYBOARD_AGENT_ID = "drama_storyboard";

export const STORYBOARD_SYSTEM = `${SHARED_RULES}

# 本阶段任务
仅在定稿已存在时，把 5分钟短剧-剧本定稿.md 拆成分镜。不得改剧情、不得加新设定、不得把对白写进画面生成词。

# 必读
format-storyboard.md 与定稿全文。

# 输出（落盘 5分钟短剧-分镜脚本.md）
每镜一行字段：镜号 | 景别 | 画面（无对白文字） | 对白/旁白 | 时长 | 音效
必须覆盖定稿全部场次，最后一镜对应定稿最后一场。每镜 3–12 秒，合计 280–320 秒。
禁止把【待确认】写入落盘正文。
【待确认】只问要不要调整某一段镜头密度`;
