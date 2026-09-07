import { describe, expect, it } from "vitest";
import { createEmptyStoryboardShot, isStoryboardRoundTripSafe, normalizeStoryboardAnchorReferences, parseStoryboardShots, serializeStoryboard, storyboardShotsToGenerationItems } from "./storyboard";

const MARKDOWN = `# 视频分镜

## 第1镜

### 场次
外景 海边 清晨

### 景别
中景

### 构图
主体居中，海平线位于上三分线

### 光线
清晨侧逆光，柔和阴影

### 运镜
低机位缓慢后退跟拍

### 画面动作
白色小狗沿湿润沙滩跑近

### 情绪
轻快

### 时长
3秒

### 起始状态
小狗位于远处沙滩中央

### 动作过程
小狗持续向镜头跑近，脚边溅起少量水花

### 结束状态
小狗到达镜头前方

### 承接镜头
无

### 画风锚
style:film:v1

### 场景锚
scene:beach:v1

### 角色锚
character:dog:v1

### 道具锚
无

### 参考方式
text

### 参考资产
无

### 来源对白
无

### 视频 Prompt
电影写实风格，清晨海边，固定使用 style:film:v1、scene:beach:v1、character:dog:v1 的可见特征，白色小狗沿湿润沙滩向镜头跑近，低机位缓慢后退跟拍，动作自然连贯，无字幕无文字。

## 第2镜

### 场次
外景 海边 清晨

### 景别
近景

### 构图
主体偏左，右侧保留海面空间

### 光线
清晨侧逆光，柔和阴影

### 运镜
侧面缓慢环绕

### 画面动作
白色小狗停在浅水边抬头看海

### 情绪
好奇

### 时长
5秒

### 起始状态
小狗已到达镜头前方

### 动作过程
小狗减速停下，缓慢转头看向海面

### 结束状态
小狗面向海面静止

### 承接镜头
第1镜

### 画风锚
style:film:v1

### 场景锚
scene:beach:v1

### 角色锚
character:dog:v1

### 道具锚
无

### 参考方式
text

### 参考资产
无

### 来源对白
无

### 视频 Prompt
承接上一镜，保持 style:film:v1、scene:beach:v1、character:dog:v1 的外观和环境一致，白色小狗在浅水边停下并抬头看向海面，镜头从侧面缓慢环绕，无字幕无文字。`;

describe("video storyboard Markdown", () => {
  it("parses the only supported Markdown contract", () => {
    const shots = parseStoryboardShots(MARKDOWN);
    expect(shots).toHaveLength(2);
    expect(shots[0]).toMatchObject({ shotNo: 1, durationS: 3, styleAnchor: "style:film:v1", sceneAnchor: "scene:beach:v1", characterAnchors: ["character:dog:v1"] });
    expect(shots[1]).toMatchObject({ shotNo: 2, durationS: 5, continuityFrom: 1 });
    expect(parseStoryboardShots('{"shots":[]}')).toEqual([]);
  });

  it("rejects missing headings and non-contiguous shot numbers", () => {
    expect(parseStoryboardShots(MARKDOWN.replace("### 画风锚", "### 画风"))).toEqual([]);
    expect(parseStoryboardShots(MARKDOWN.replace("## 第2镜", "## 第3镜"))).toEqual([]);
  });

  it("rejects duplicate or unknown fields instead of silently using the last value", () => {
    expect(parseStoryboardShots(MARKDOWN.replace("### 视频 Prompt\n", "### 视频 Prompt\n错误前值\n\n### 视频 Prompt\n"))).toEqual([]);
    expect(parseStoryboardShots(MARKDOWN.replace("### 时长\n3秒", "### 时长\n2秒\n\n### 时长\n3秒"))).toEqual([]);
    expect(parseStoryboardShots(MARKDOWN.replace("## 第2镜", "## 制作备注\n不能混入镜头块\n\n## 第2镜"))).toEqual([]);
    expect(parseStoryboardShots(MARKDOWN.replace("### 情绪\n轻快", "### 未知字段\n内容\n\n### 情绪\n轻快"))).toEqual([]);
  });

  it("round-trips Markdown and preserves per-shot durations", () => {
    const shots = parseStoryboardShots(MARKDOWN);
    expect(parseStoryboardShots(serializeStoryboard(shots))).toEqual(shots);
    expect(storyboardShotsToGenerationItems(shots)).toEqual([
      { shotNo: 1, durationS: 3, prompt: shots[0].videoPrompt, anchorIds: ["style:film:v1", "scene:beach:v1", "character:dog:v1"], continuityFrom: null, referenceStrategy: "text", referenceAssetIds: [] },
      { shotNo: 2, durationS: 5, prompt: shots[1].videoPrompt, anchorIds: ["style:film:v1", "scene:beach:v1", "character:dog:v1"], continuityFrom: 1, referenceStrategy: "text", referenceAssetIds: [] },
    ]);
  });

  it("expands multiple fixed and state scene anchors into generation provenance", () => {
    const shot = parseStoryboardShots(MARKDOWN)[0];
    shot.sceneAnchor = "scene:beach:v1, state:scene:beach:ch01:storm:v1";
    expect(storyboardShotsToGenerationItems([shot])[0].anchorIds).toContain("state:scene:beach:ch01:storm:v1");
  });

  it("creates a complete editable shot", () => {
    expect(createEmptyStoryboardShot(parseStoryboardShots(MARKDOWN))).toMatchObject({ shotNo: 3, durationS: 5, continuityFrom: 2, styleAnchor: "style:main:v1" });
  });

  it("ignores visual separators but refuses unknown headings for structured editing", () => {
    const markdown = MARKDOWN.replace("\n\n## 第2镜", "\n\n---\n\n## 第2镜");
    expect(isStoryboardRoundTripSafe(markdown)).toBe(true);
    expect(parseStoryboardShots(markdown)[0].videoPrompt).not.toContain("---");
    expect(isStoryboardRoundTripSafe(`${markdown}\n\n### 人工备注\n不要丢失`)).toBe(false);
  });

  it("normalizes anchor IDs that were incorrectly placed in reference assets", () => {
    const shot = createEmptyStoryboardShot([]);
    shot.referenceStrategy = "reference";
    shot.referenceAssetIds = ["character:dog:v1", "state:scene:beach:ch01:day:v1"];
    const normalized = normalizeStoryboardAnchorReferences(serializeStoryboard([shot]));
    expect(normalized.correctedShotNos).toEqual([1]);
    expect(parseStoryboardShots(normalized.markdown)[0]).toMatchObject({ referenceStrategy: "text", referenceAssetIds: [] });
    shot.referenceAssetIds = ["image_asset_123"];
    expect(normalizeStoryboardAnchorReferences(serializeStoryboard([shot])).correctedShotNos).toEqual([]);
  });
});
