export type VideoReferenceStrategy = "text" | "first_frame" | "reference";

export interface StoryboardShot {
  shotNo: number;
  scene: string;
  shotType: string;
  composition: string;
  light: string;
  camera: string;
  action: string;
  emotion: string;
  durationS: number;
  startState: string;
  actionProgress: string;
  endState: string;
  continuityFrom: number | null;
  styleAnchor: string;
  sceneAnchor: string;
  characterAnchors: string[];
  propAnchors: string[];
  referenceStrategy: VideoReferenceStrategy;
  referenceAssetIds: string[];
  sourceDialogue: string;
  videoPrompt: string;
}

const FIELD_LABELS = {
  scene: "场次",
  shotType: "景别",
  composition: "构图",
  light: "光线",
  camera: "运镜",
  action: "画面动作",
  emotion: "情绪",
  durationS: "时长",
  startState: "起始状态",
  actionProgress: "动作过程",
  endState: "结束状态",
  continuityFrom: "承接镜头",
  styleAnchor: "画风锚",
  sceneAnchor: "场景锚",
  characterAnchors: "角色锚",
  propAnchors: "道具锚",
  referenceStrategy: "参考方式",
  referenceAssetIds: "参考资产",
  sourceDialogue: "来源对白",
  videoPrompt: "视频 Prompt",
} as const;

type FieldKey = keyof typeof FIELD_LABELS;
const REQUIRED_FIELDS = Object.keys(FIELD_LABELS) as FieldKey[];

function list(value: string): string[] {
  const trimmed = value.trim();
  if (!trimmed || trimmed === "无") return [];
  return trimmed.split(/[,，、\n]/).map((item) => item.trim()).filter(Boolean);
}

function parseFields(block: string): Map<string, string> | null {
  const knownLabels = new Set<string>(Object.values(FIELD_LABELS));
  const headings = [...block.matchAll(/^###\s+([^\n]+)\s*$/gm)];
  // Only known field labels delimit fields. Unknown `###` headings (for example
  // a structured sub-heading inside the free-form video Prompt) stay part of the
  // preceding field's text instead of invalidating the whole document.
  const boundaries = headings.filter((match) => knownLabels.has(match[1].trim()));
  const values = new Map<string, string>();
  for (let index = 0; index < boundaries.length; index += 1) {
    const match = boundaries[index];
    const label = match[1].trim();
    if (values.has(label)) return null;
    const start = (match.index ?? 0) + match[0].length;
    const end = boundaries[index + 1]?.index ?? block.length;
    values.set(label, block.slice(start, end).trim().replace(/\n---\s*$/u, "").trim());
  }
  if (values.size !== REQUIRED_FIELDS.length) return null;
  return values;
}

export function isStoryboardRoundTripSafe(markdown: string): boolean {
  const shots = parseStoryboardShots(markdown);
  if (!shots.length) return false;
  const headings = [...markdown.matchAll(/^(#{1,3})\s+(.+)$/gm)];
  if (headings.some((match) => {
    const level = match[1].length;
    const label = match[2].trim();
    if (level === 1) return label !== "视频分镜";
    if (level === 2) return !/^第\s*\d+\s*镜$/u.test(label);
    // Level-3 headings are either known fields (validated by the parser) or
    // free-form content inside the preceding field, which serialization keeps.
    return false;
  })) return false;
  const firstShot = markdown.search(/^##\s+第\s*\d+\s*镜\s*$/m);
  const titleEnd = markdown.search(/\n/);
  if (firstShot < 0) return false;
  const preamble = markdown.slice(titleEnd < 0 ? 0 : titleEnd + 1, firstShot).replace(/^---\s*$/gm, "").trim();
  return !preamble;
}

export function parseStoryboardShots(markdown: string): StoryboardShot[] {
  const text = markdown.replace(/\r\n?/g, "\n").trim();
  if (!/^#\s+视频分镜\s*$/m.test(text)) return [];
  const headings = [...text.matchAll(/^##\s+第\s*(\d+)\s*镜\s*$/gm)];
  if (!headings.length) return [];
  const allLevelTwo = [...text.matchAll(/^##\s+([^\n]+)\s*$/gm)];
  if (allLevelTwo.length !== headings.length || allLevelTwo.some((match) => !/^第\s*\d+\s*镜$/u.test(match[1].trim()))) return [];
  const firstShotIndex = headings[0].index ?? 0;
  if (/^###\s+/m.test(text.slice(0, firstShotIndex))) return [];
  const shots: StoryboardShot[] = [];
  for (let index = 0; index < headings.length; index += 1) {
    const shotNo = Number(headings[index][1]);
    const start = (headings[index].index ?? 0) + headings[index][0].length;
    const end = headings[index + 1]?.index ?? text.length;
    const fields = parseFields(text.slice(start, end));
    if (!fields) return [];
    if (!Number.isSafeInteger(shotNo) || shotNo !== index + 1) return [];
    if (REQUIRED_FIELDS.some((key) => !fields.has(FIELD_LABELS[key]))) return [];
    const durationMatch = fields.get(FIELD_LABELS.durationS)!.match(/^(\d+)\s*秒?$/);
    const durationS = durationMatch ? Number(durationMatch[1]) : 0;
    if (!Number.isSafeInteger(durationS) || durationS <= 0) return [];
    const strategy = fields.get(FIELD_LABELS.referenceStrategy)!.trim();
    if (strategy !== "text" && strategy !== "first_frame" && strategy !== "reference") return [];
    const continuityText = fields.get(FIELD_LABELS.continuityFrom)!.trim();
    const continuityFrom = continuityText === "无" ? null : Number(continuityText.match(/\d+/)?.[0] ?? 0);
    if (continuityFrom !== null && (!Number.isSafeInteger(continuityFrom) || continuityFrom < 1 || continuityFrom >= shotNo)) return [];
    const requiredText = (key: FieldKey) => fields.get(FIELD_LABELS[key])!.trim();
    const shot: StoryboardShot = {
      shotNo,
      scene: requiredText("scene"),
      shotType: requiredText("shotType"),
      composition: requiredText("composition"),
      light: requiredText("light"),
      camera: requiredText("camera"),
      action: requiredText("action"),
      emotion: requiredText("emotion"),
      durationS,
      startState: requiredText("startState"),
      actionProgress: requiredText("actionProgress"),
      endState: requiredText("endState"),
      continuityFrom,
      styleAnchor: requiredText("styleAnchor"),
      sceneAnchor: requiredText("sceneAnchor"),
      characterAnchors: list(requiredText("characterAnchors")),
      propAnchors: list(requiredText("propAnchors")),
      referenceStrategy: strategy,
      referenceAssetIds: list(requiredText("referenceAssetIds")),
      sourceDialogue: requiredText("sourceDialogue") === "无" ? "" : requiredText("sourceDialogue"),
      videoPrompt: requiredText("videoPrompt"),
    };
    if ([shot.scene, shot.shotType, shot.composition, shot.light, shot.camera, shot.action, shot.emotion, shot.startState, shot.actionProgress, shot.endState, shot.styleAnchor, shot.sceneAnchor, shot.videoPrompt].some((value) => !value)) return [];
    shots.push(shot);
  }
  return shots;
}

function shownList(values: string[]): string {
  return values.length ? values.join("、") : "无";
}

export function serializeStoryboard(shots: StoryboardShot[]): string {
  const ordered = [...shots].sort((a, b) => a.shotNo - b.shotNo);
  const sections = ordered.map((shot, index) => {
    const normalized = { ...shot, shotNo: index + 1 };
    return `## 第${normalized.shotNo}镜

### 场次
${normalized.scene}

### 景别
${normalized.shotType}

### 构图
${normalized.composition}

### 光线
${normalized.light}

### 运镜
${normalized.camera}

### 画面动作
${normalized.action}

### 情绪
${normalized.emotion}

### 时长
${normalized.durationS}秒

### 起始状态
${normalized.startState}

### 动作过程
${normalized.actionProgress}

### 结束状态
${normalized.endState}

### 承接镜头
${normalized.continuityFrom === null ? "无" : `第${normalized.continuityFrom}镜`}

### 画风锚
${normalized.styleAnchor}

### 场景锚
${normalized.sceneAnchor}

### 角色锚
${shownList(normalized.characterAnchors)}

### 道具锚
${shownList(normalized.propAnchors)}

### 参考方式
${normalized.referenceStrategy}

### 参考资产
${shownList(normalized.referenceAssetIds)}

### 来源对白
${normalized.sourceDialogue || "无"}

### 视频 Prompt
${normalized.videoPrompt}`;
  });
  return `# 视频分镜

${sections.join("\n\n")}`.trim();
}

export function normalizeStoryboardAnchorReferences(markdown: string): { markdown: string; correctedShotNos: number[] } {
  const shots = parseStoryboardShots(markdown);
  if (!shots.length) return { markdown, correctedShotNos: [] };
  const correctedShotNos: number[] = [];
  const normalized = shots.map((shot) => {
    if (shot.referenceStrategy === "text") return shot;
    const onlyAnchors = shot.referenceAssetIds.length > 0
      && shot.referenceAssetIds.every((id) => /^(?:style|character|scene|prop|state):/.test(id));
    if (shot.referenceAssetIds.length > 0 && !onlyAnchors) return shot;
    correctedShotNos.push(shot.shotNo);
    return { ...shot, referenceStrategy: "text" as const, referenceAssetIds: [] };
  });
  return correctedShotNos.length ? { markdown: serializeStoryboard(normalized), correctedShotNos } : { markdown, correctedShotNos };
}

export function storyboardShotsToGenerationItems(shots: StoryboardShot[]): Array<{ shotNo: number; prompt: string; durationS: number; anchorIds: string[]; continuityFrom: number | null; referenceStrategy: VideoReferenceStrategy; referenceAssetIds: string[] }> {
  return [...shots].sort((a, b) => a.shotNo - b.shotNo).map((shot) => ({
    shotNo: shot.shotNo,
    prompt: shot.videoPrompt.trim(),
    durationS: shot.durationS,
    anchorIds: [shot.styleAnchor, ...list(shot.sceneAnchor), ...shot.characterAnchors, ...shot.propAnchors],
    continuityFrom: shot.continuityFrom,
    referenceStrategy: shot.referenceStrategy,
    referenceAssetIds: shot.referenceAssetIds,
  })).filter((item) => item.prompt);
}

export function createEmptyStoryboardShot(shots: StoryboardShot[]): StoryboardShot {
  const shotNo = shots.length + 1;
  return {
    shotNo, scene: "待填写", shotType: "中景", composition: "主体居中", light: "自然光", camera: "固定镜头",
    action: "待填写", emotion: "平静", durationS: 5, startState: "待填写", actionProgress: "待填写", endState: "待填写",
    continuityFrom: shotNo === 1 ? null : shotNo - 1, styleAnchor: "style:main:v1", sceneAnchor: "scene:main:v1", characterAnchors: [], propAnchors: [], referenceStrategy: "text", referenceAssetIds: [], sourceDialogue: "", videoPrompt: "待填写视频 Prompt",
  };
}

export function resequenceStoryboardShots(shots: StoryboardShot[]): StoryboardShot[] {
  return shots.map((shot, index) => ({ ...shot, shotNo: index + 1, continuityFrom: index === 0 ? null : shot.continuityFrom === null ? null : Math.min(shot.continuityFrom, index) }));
}
