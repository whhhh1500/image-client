import { describe, expect, it } from "vitest";
import { createEmptyStoryboardShot, serializeStoryboard } from "./storyboard";
import { deterministicVideoQualityIssues, parseVideoQcVerdict, parseVideoQualityReview, storyboardProductionIssues, videoQualityReviewSystem } from "./qualityReview";

function fullReview(conclusion = "通过", score = 86, blockers = "无", extra = ""): string {
  return `# 产物质量审查
【结论】${conclusion}
【总分】${score}
【剧情吸引力】78 | 依据
【视觉独特性】76 | 依据
【原文忠实度】92 | 依据
【人物与情感】85 | 依据
【可执行性】88 | 依据
【一致性】90 | 依据
【亮点】具体亮点
【阻断问题】${blockers}
【一般问题】无
【改进建议】保持克制${extra}`;
}

describe("video product quality review", () => {
  it("fails closed when a review claims pass with a low score or blockers", () => {
    const low = parseVideoQualityReview(fullReview("通过", 79));
    expect(low.status).toBe("needs_changes");
    const blocked = parseVideoQualityReview(fullReview("通过", 90, "第1场 | 越界续写"));
    expect(blocked.status).toBe("needs_changes");
  });

  it("rejects provider refusal text instead of treating it as a review", () => {
    expect(() => parseVideoQualityReview("The prompt could not be submitted. Prohibited Use policy.")).toThrow("拒绝");
  });

  it("rejects a structurally incomplete review even when it claims pass", () => {
    expect(() => parseVideoQualityReview("【结论】通过\n【总分】90\n【阻断问题】无")).toThrow("缺少固定字段");
    expect(() => parseVideoQualityReview(`${fullReview()}\n【结论】需修改`)).toThrow("重复字段");
  });

  it("accepts only an explicit high-scoring review with no blockers", () => {
    const review = parseVideoQualityReview(fullReview());
    expect(review).toMatchObject({ status: "passed", score: 86, blockingIssues: "无" });
  });

  it("keeps persisted deterministic blockers in future optimization suggestions", () => {
    const review = parseVideoQualityReview(fullReview("通过", 90, "无", "\n【程序阻断】第2镜画幅错误"));
    expect(review.status).toBe("needs_changes");
    expect(review.blockingIssues).toContain("画幅错误");
    expect(review.suggestions).toContain("程序阻断");
  });

  it("detects project aspect ratio conflicts before semantic review", () => {
    const shot = createEmptyStoryboardShot([]);
    shot.videoPrompt = "5秒，16:9画幅，人物向前走。";
    expect(deterministicVideoQualityIssues("storyboard", serializeStoryboard([shot]), "9:16")).toContain("第1镜的视频 Prompt 画幅与项目 9:16 冲突");
  });

  it("reviews story quality and source fidelity rather than only formatting", () => {
    const system = videoQualityReviewSystem("script");
    expect(system).toContain("越过原文结尾");
    expect(system).toContain("模板腔");
    expect(system).toContain("物理可信度");
  });

  it("does not accept a QC report by substring when blockers remain", () => {
    const qc = (blockers = "无", conclusion = "可生成", quality = 82) => `【质量评分】${quality}\n【原文忠实度】90 | 依据\n【剧情吸引力】70 | 依据\n【视觉独特性】60 | 依据\n【可执行性】85 | 依据\n【阻断问题】${blockers}\n【一般问题】无\n【结论】${conclusion}`;
    const report = qc("第1镜 | 画幅错误");
    expect(parseVideoQcVerdict(report)).toMatchObject({ passed: false, conclusion: "可生成" });
    expect(parseVideoQcVerdict(qc()).passed).toBe(true);
    expect(parseVideoQcVerdict(`${qc()}\n【阻断问题】第2镜错误`).passed).toBe(false);
    expect(parseVideoQcVerdict(`${qc()}\n【结论】需修改`).passed).toBe(false);
    expect(parseVideoQcVerdict(qc("无", "可生成", 45)).passed).toBe(false);
  });

  it("blocks missing anchors and reference assets before video generation", () => {
    const shot = createEmptyStoryboardShot([]);
    shot.styleAnchor = "style:missing:v1";
    shot.referenceStrategy = "first_frame";
    shot.referenceAssetIds = ["image-missing", "character:dog:v1"];
    const issues = storyboardProductionIssues({ storyboardMarkdown: serializeStoryboard([shot]), anchorMarkdown: "# 视频锚点", availableAssetIds: [] });
    expect(issues.join("\n")).toContain("不存在的锚点");
    expect(issues.join("\n")).toContain("参考资产不存在");
    expect(issues.join("\n")).toContain("文本锚点误当成媒体参考资产");
  });

  it("requires provider-usable HTTPS media for per-shot reference modes", () => {
    const shot = createEmptyStoryboardShot([]);
    shot.referenceStrategy = "first_frame";
    shot.referenceAssetIds = ["image-local"];
    const issues = storyboardProductionIssues({ storyboardMarkdown: serializeStoryboard([shot]), anchorMarkdown: "style:main:v1 | 风格\nscene:main:v1 | 场景", availableAssets: [{ id: "image-local", kind: "image", path: "C:/local.png" }] });
    expect(issues.join("\n")).toContain("不是公网 HTTPS");
  });

  it("recognizes state anchors and rejects pseudo-physics plus evidence-free QC", () => {
    const shot = createEmptyStoryboardShot([]);
    shot.sceneAnchor = "scene:road:v1, state:scene:road:ch01:chaos:v1";
    const anchors = "scene:road:v1 | 路口\nstate:scene:road:ch01:chaos:v1 | ch01 | 路口 | 混乱";
    expect(storyboardProductionIssues({ storyboardMarkdown: serializeStoryboard([shot]), anchorMarkdown: anchors }).join("\n")).not.toContain("scene:road");
    expect(deterministicVideoQualityIssues("script", "反作用力让他滞留在原地，把孩子推飞数米")).not.toHaveLength(0);
    expect(deterministicVideoQualityIssues("script", "少年飞步跨越数米跑到妹妹身边")).toHaveLength(0);
    expect(deterministicVideoQualityIssues("qc", "【阻断问题】无\n【一般问题】无\n【结论】可生成").join("\n")).toContain("过短");
    const inflated = "【质量评分】92\n【原文忠实度】95\n【剧情吸引力】88\n【视觉独特性】82 | 经典常见模板\n【硬规则结果】通过\n【锚点检查】通过\n【逐镜检查】第1镜通过\n【阻断问题】无\n【一般问题】无\n【结论】可生成".padEnd(520, "说明");
    expect(deterministicVideoQualityIssues("qc", inflated).join("\n")).toContain("评分校准不可信");
  });

  it("blocks source-fact reversals and POV self-face anchors", () => {
    expect(deterministicVideoQualityIssues("script", "泥头车锁死急刹后停住", "16:9", "泥头车制动完全失灵").join("\n")).toContain("改变了核心事实");
    const shot = createEmptyStoryboardShot([]);
    shot.shotType = "张帅主观视点特写";
    shot.characterAnchors = ["character:张帅:v1", "state:character:张帅:ch01:fading:v1", "character:小萌:v1"];
    const anchors = "character:张帅:v1 | 张帅基础外貌\nstate:character:张帅:ch01:fading:v1 | 第1章 | 张帅 | 面部神情释然，嘴角微笑\ncharacter:小萌:v1 | 小萌基础外貌";
    expect(storyboardProductionIssues({ storyboardMarkdown: serializeStoryboard([shot]), anchorMarkdown: anchors }).join("\n")).toContain("第一人称主观镜头");
  });
});
