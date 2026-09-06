import { describe, expect, it } from "vitest";
import type { LibAsset } from "../../store/useLibraryStore";
import {
  DRAMA_ARTIFACTS,
  DRAMA_STAGES,
  canStoryboard,
  assessDramaStage,
  assessDramaStageBundle,
  draftComplete,
  extractFinalScript,
  firstOpenStage,
  mergeDraft,
  nextMissingAct,
  presentActs,
  reviewPassed,
  reviewRewindStage,
  shouldPersistArtifact,
  shouldPersistPartialArtifact,
  stagePartialArtifactTitle,
  stageComplete,
  stagePartial,
  stageUnlocked,
  stripDeliveryMeta,
  looksTruncated,
  planDramaStagePersistence,
} from "./stages";

function doc(title: string, text = "ok", projectId: string | null = "p1"): LibAsset {
  return {
    asset: { id: title, kind: "text", path: `${title}.md` },
    source: title,
    projectId: projectId ?? undefined,
    createdAt: Date.now(),
    params: { title, text, documentType: "drama" },
  };
}

const ANCHOR_V1 = `【赛道】都市日常
【一句话故事】谁必须留下，否则失去孩子
【核心立意】留下不是投降
【主结构原型】期限倒计时
【主叙事重心】角色向
【视觉形态】AI 真人剧`;

const ANCHOR_V3 = `${ANCHOR_V1}
【30秒开场】3秒看见人；10秒看懂处境；30秒不能退
【结尾兑现】回收开场的门卡
【原型执行表】启动 / 升级 / 假高潮 / 结尾`;

const WORLD = `【世界法则】夜班病房不能关灯
【铁三角·主角】护士林见
【一致性禁忌】不许新增第四方势力`;

const BEATS = `【卡点大纲】
0:00 | 病房 | 新信息 | 留下 | 门卡`;

const DRAFT_ACT1 = `【元数据】片名
第一幕
内 病房 夜
她把门口的门卡按进掌心。`;

const DRAFT_ALL = `【元数据】片名
第一幕
开场入戏
第二幕
加压
第三幕
假高潮
第四幕
兑现结尾`;

const REVIEW_PASS = `【开篇】通过
证据：开场看见人
修改方向：无
【冲突】通过
证据：不能退
修改方向：无
【结尾】通过
证据：回收门卡
修改方向：无
【总评】允许定稿
# 定稿
完整正式剧本`;

const SIX_STAGE_RESPONSE = `${ANCHOR_V3}

${WORLD}

${BEATS}

${DRAFT_ALL}

${REVIEW_PASS}`;

describe("five-minute drama stage gates", () => {
  it("does not treat stage-1 anchor as stage-3 engine lock", () => {
    const assets = [doc(DRAMA_ARTIFACTS.anchor, ANCHOR_V1), doc(DRAMA_ARTIFACTS.world, WORLD)];
    expect(stageComplete(assets, "p1", DRAMA_STAGES[0])).toBe(true);
    expect(stageComplete(assets, "p1", DRAMA_STAGES[2])).toBe(false);
    expect(stageUnlocked(assets, "p1", DRAMA_STAGES[3])).toBe(false);
  });

  it("ignores artifacts without projectId when gating another project's stages", () => {
    const legacy = [
      doc(DRAMA_ARTIFACTS.anchor, ANCHOR_V3, null),
      doc(DRAMA_ARTIFACTS.world, WORLD, null),
      doc(DRAMA_ARTIFACTS.beats, BEATS, null),
    ];
    expect(stageComplete(legacy, "p1", DRAMA_STAGES[0])).toBe(false);
    expect(stageUnlocked(legacy, "p1", DRAMA_STAGES[1])).toBe(false);
    expect(canStoryboard(legacy, "p1")).toBe(false);
    // 无项目上下文时（projectId 为 null）才允许匹配无项目文档。
    expect(stageUnlocked(legacy, null, DRAMA_STAGES[3])).toBe(true);
  });

  it("unlocks later stages only after previous artifacts are actually complete", () => {
    expect(stageUnlocked([], "p1", DRAMA_STAGES[0])).toBe(true);
    expect(stageUnlocked([], "p1", DRAMA_STAGES[1])).toBe(false);
    expect(stageUnlocked([doc(DRAMA_ARTIFACTS.anchor, ANCHOR_V1)], "p1", DRAMA_STAGES[1])).toBe(true);

    const throughEngine = [
      doc(DRAMA_ARTIFACTS.anchor, ANCHOR_V3),
      doc(DRAMA_ARTIFACTS.world, WORLD),
    ];
    expect(stageUnlocked(throughEngine, "p1", DRAMA_STAGES[3])).toBe(true);
    expect(stageUnlocked(throughEngine, "p1", DRAMA_STAGES[4])).toBe(false);
  });

  it("keeps review locked until the draft has four acts", () => {
    const base = [
      doc(DRAMA_ARTIFACTS.anchor, ANCHOR_V3),
      doc(DRAMA_ARTIFACTS.world, WORLD),
      doc(DRAMA_ARTIFACTS.beats, BEATS),
      doc(DRAMA_ARTIFACTS.draft, DRAFT_ACT1),
    ];
    expect(draftComplete(DRAFT_ACT1)).toBe(false);
    expect(stageComplete(base, "p1", DRAMA_STAGES[4])).toBe(false);
    expect(stageUnlocked(base, "p1", DRAMA_STAGES[5])).toBe(false);

    const full = [...base.slice(0, 3), doc(DRAMA_ARTIFACTS.draft, DRAFT_ALL)];
    expect(stageComplete(full, "p1", DRAMA_STAGES[4])).toBe(true);
    expect(stageUnlocked(full, "p1", DRAMA_STAGES[5])).toBe(true);
  });

  it("requires three explicit passes plus a final script before storyboard", () => {
    expect(reviewPassed("【开篇】通过\n【冲突】待修\n【结尾】通过")).toBe(false);
    expect(reviewPassed(REVIEW_PASS)).toBe(true);
    expect(reviewPassed("开篇清晰：通过\n冲突来源：通过\n结尾钩子：通过")).toBe(false);
    expect(canStoryboard([doc(DRAMA_ARTIFACTS.draft, DRAFT_ALL)], "p1")).toBe(false);
    expect(canStoryboard([doc(DRAMA_ARTIFACTS.final, "定稿")], "p1")).toBe(false);
    expect(canStoryboard([
      doc(DRAMA_ARTIFACTS.review, REVIEW_PASS),
      doc(DRAMA_ARTIFACTS.final, extractFinalScript(REVIEW_PASS)),
    ], "p1")).toBe(true);
  });

  it("rewinds rewrite verdicts to the matching earlier stage", () => {
    expect(reviewRewindStage("【开篇】重写\n【冲突】通过\n【结尾】通过")).toBe("drama_engine");
    expect(reviewRewindStage("【开篇】通过\n【冲突】重写\n【结尾】通过")).toBe("drama_world");
    expect(reviewRewindStage("【开篇】通过\n【冲突】通过\n【结尾】待修")).toBeNull();
  });

  it("merges one-act draft updates without dropping earlier acts", () => {
    const merged = mergeDraft(DRAFT_ACT1, "第二幕\n加压成立");
    expect(presentActs(merged)).toEqual([1, 2]);
    expect(merged).toContain("门卡");
    expect(nextMissingAct(merged)).toBe(3);
    expect(extractFinalScript(REVIEW_PASS)).toContain("完整正式剧本");
    expect(firstOpenStage([], "p1").id).toBe("drama_position");
    expect(shouldPersistArtifact(DRAMA_STAGES[0], "【待确认】选都市还是古风？")).toBe(false);
    expect(shouldPersistArtifact(DRAMA_STAGES[0], ANCHOR_V1)).toBe(true);
    expect(shouldPersistArtifact(DRAMA_STAGES[4], "【待确认】要不要写第二幕？")).toBe(false);
    expect(shouldPersistArtifact(DRAMA_STAGES[4], DRAFT_ACT1)).toBe(false);
    expect(shouldPersistPartialArtifact(DRAMA_STAGES[4], DRAFT_ACT1)).toBe(true);
    expect(stripDeliveryMeta(`${DRAFT_ACT1}\n\n【待确认】写下幕？`)).not.toContain("待确认");
    expect(looksTruncated("连一秒")).toBe(true);
    expect(looksTruncated("完整结尾。")).toBe(false);
  });

  it("recognizes a complete six-stage response without treating arbitrary prose as a stage", () => {
    const bundle = assessDramaStageBundle(SIX_STAGE_RESPONSE, "drama_position");
    const persistence = planDramaStagePersistence(SIX_STAGE_RESPONSE, "drama_position");
    expect(bundle.completeStageIds).toEqual(DRAMA_STAGES.map((stage) => stage.id));
    expect(bundle.partialStageIds).toEqual([]);
    expect(bundle.assessments.every((assessment) => assessment.missing.length === 0)).toBe(true);
    expect(persistence.adoptedStageIds).toEqual(DRAMA_STAGES.map((stage) => stage.id));
  });

  it("keeps an incomplete later stage as a named partial draft with its missing markers", () => {
    const incompleteWorld = "【世界法则】夜班病房不能关灯";
    const assessment = assessDramaStage(DRAMA_STAGES[1], incompleteWorld);
    const bundle = assessDramaStageBundle(`${ANCHOR_V1}\n${incompleteWorld}`, "drama_position");
    const persistence = planDramaStagePersistence(`${ANCHOR_V1}\n${incompleteWorld}`, "drama_position");

    expect(assessment.complete).toBe(false);
    expect(assessment.partial).toBe(true);
    expect(assessment.missing).toEqual(["【铁三角·主角】", "【一致性禁忌】"]);
    expect(shouldPersistArtifact(DRAMA_STAGES[1], incompleteWorld)).toBe(false);
    expect(shouldPersistPartialArtifact(DRAMA_STAGES[1], incompleteWorld)).toBe(true);
    expect(bundle.completeStageIds).toEqual(["drama_position"]);
    expect(bundle.partialStageIds).toEqual(["drama_world"]);
    expect(persistence.adoptedStageIds).toEqual(["drama_position"]);
    expect(stagePartialArtifactTitle(DRAMA_STAGES[1])).toContain("阶段2");
    expect(stagePartial([doc(stagePartialArtifactTitle(DRAMA_STAGES[1]), incompleteWorld)], "p1", DRAMA_STAGES[1])?.missing)
      .toEqual(["【铁三角·主角】", "【一致性禁忌】"]);
  });

  it("preserves the existing one-stage completion behavior", () => {
    const assessment = assessDramaStage(DRAMA_STAGES[0], ANCHOR_V1);
    expect(assessment.complete).toBe(true);
    expect(assessment.partial).toBe(false);
    expect(assessment.missing).toEqual([]);
    expect(assessDramaStageBundle(ANCHOR_V1, "drama_position").completeStageIds).toEqual(["drama_position"]);
  });

  it("evaluates an incremental four-act draft from its merged partial draft", () => {
    const firstThree = mergeDraft(mergeDraft(DRAFT_ACT1, "第二幕\n加压"), "第三幕\n假高潮");
    const candidate = mergeDraft(firstThree, "第四幕\n兑现");
    expect(assessDramaStage(DRAMA_STAGES[4], firstThree).missing).toEqual(["第4幕"]);
    expect(assessDramaStage(DRAMA_STAGES[4], candidate).complete).toBe(true);
    expect(planDramaStagePersistence(candidate, "drama_draft").adoptedStageIds).toEqual(["drama_draft"]);
  });
});
