import { describe, expect, it } from "vitest";
import type { LibAsset } from "../../store/useLibraryStore";
import { buildVideoOptimizationRequest, callVideoLlmWithSafetyRetry, cleanAndValidateVideoMarkdown, prepareVideoAgentContext, previousChapterAnchorAssets, VIDEO_REVIEW_POLICY_VERSION, videoOptimizationSystem, videoWorkflowIdForSource } from "./markdownWorkflow";
import { serializeStoryboard, createEmptyStoryboardShot } from "./storyboard";

function anchor(id: string, chapterNo: number, version: number, createdAt: number): LibAsset {
  const text = "# 视频锚点";
  return {
    asset: { id, kind: "text", path: `${id}.md` }, source: `第${chapterNo}章锚点`, projectId: "p", createdAt,
    params: {
      text, documentType: "consistency", documentId: `anchors-${chapterNo}`, version, agentId: "consistency", novelWorkId: "book", chapterNo,
      agentReviewStatus: "passed", agentReviewedText: text,
      agentReviewPolicyVersion: VIDEO_REVIEW_POLICY_VERSION,
      agentReview: "# 产物质量审查\n【结论】通过\n【总分】86\n【剧情吸引力】78\n【视觉独特性】76\n【原文忠实度】90\n【人物与情感】85\n【可执行性】88\n【一致性】90\n【亮点】具体\n【阻断问题】无\n【一般问题】无\n【改进建议】保持",
    },
  };
}

describe("video Markdown workflow", () => {
  it("builds stage-aware optimization prompts without saving semantics", () => {
    const system = videoOptimizationSystem("anchors", "BASE");
    expect(system).toContain("固定锚定");
    expect(system).toContain("剧情锚点");
    expect(system).toContain("完整锚点快照");
    const request = buildVideoOptimizationRequest({ instruction: "保留伤势", markdown: "# 视频锚点", projectContext: "16:9", dependencyContext: "上一章", workspaceContext: "视频分镜 v2\n视频结果：第1镜" });
    expect(request).toContain("已保存的前序资料");
    expect(request).toContain("同一视频工作区的其他已保存产物与媒体元数据");
    expect(request).toContain("视频分镜 v2");
    expect(request).toContain("当前目标 Markdown（唯一允许改写）");
  });

  it("rejects an invalid optimized storyboard without replacing the draft", () => {
    expect(() => cleanAndValidateVideoMarkdown("storyboard", "# 视频分镜\n\n字段缺失")).toThrow("原草稿已保留");
    const valid = serializeStoryboard([createEmptyStoryboardShot([])]);
    expect(cleanAndValidateVideoMarkdown("storyboard", `<think>略</think>${valid}`)).toBe(valid);
  });

  it("rejects structurally incomplete non-storyboard agent output", () => {
    expect(() => cleanAndValidateVideoMarkdown("director", "# 改编规划\n\n只有标题")).toThrow("缺少固定字段");
    expect(() => cleanAndValidateVideoMarkdown("qc", "【结论】可生成")).toThrow("质检报告缺少固定字段");
  });

  it("normalizes anchor IDs out of storyboard reference assets", () => {
    const shot = createEmptyStoryboardShot([]);
    shot.referenceStrategy = "reference";
    shot.referenceAssetIds = ["character:dog:v1"];
    const cleaned = cleanAndValidateVideoMarkdown("storyboard", serializeStoryboard([shot]));
    expect(cleaned).toContain("### 参考方式\ntext");
    expect(cleaned).toContain("### 参考资产\n无");
  });

  it("carries only the latest snapshots from the nearest previous chapters", () => {
    const assets = [anchor("c1-v1", 1, 1, 1), anchor("c1-v2", 1, 2, 2), anchor("c2", 2, 1, 3), anchor("c3", 3, 1, 4), anchor("c4", 4, 1, 5)];
    expect(previousChapterAnchorAssets(assets, { projectId: "p", novelWorkId: "book", chapterNo: 5, limit: 3 }).map((asset) => asset.asset.id)).toEqual(["c2", "c3", "c4"]);
  });

  it("does not inherit unreviewed or historical-branch anchor snapshots", () => {
    const passed = anchor("passed", 2, 2, 2);
    const unreviewed = anchor("unreviewed", 2, 3, 3);
    unreviewed.params = { ...unreviewed.params, agentReviewStatus: "pending" };
    const branch = anchor("branch", 2, 4, 4);
    branch.params = { ...branch.params, videoBranch: true };
    expect(previousChapterAnchorAssets([passed, unreviewed, branch], { projectId: "p", novelWorkId: "book", chapterNo: 3 }).map((asset) => asset.asset.id)).toEqual(["passed"]);
  });

  it("uses stable novel chapter identity instead of a revision asset id", () => {
    const source = anchor("revision", 2, 1, 1);
    source.params = { ...source.params, novelChapterId: "chapter-2" };
    expect(videoWorkflowIdForSource(source)).toBe("video:novel:book:chapter-2");
  });

  it("does not persist provider refusal text and prepares non-graphic model context", () => {
    expect(() => cleanAndValidateVideoMarkdown("script", "The prompt could not be submitted. It violates a prohibited use policy.")).toThrow("拒绝");
    const safe = prepareVideoAgentContext("血泊中传来骨骼碎裂声");
    expect(safe).toContain("非图形化");
    expect(safe).not.toContain("骨骼碎裂");
  });

  it("retries a provider refusal once with stricter non-graphic context", async () => {
    const calls: Array<{ system: string; user: string }> = [];
    const result = await callVideoLlmWithSafetyRetry(async (system, user) => {
      calls.push({ system, user });
      return calls.length === 1 ? "The prompt could not be submitted. Prohibited Use policy." : "# 视频剧本\n\n安全结果";
    }, "SYSTEM", "8岁角色遭遇撞击后濒死");
    expect(result).toContain("安全结果");
    expect(calls).toHaveLength(2);
    expect(calls[1].user).toContain("始终保持安全且不展示伤害结果");
    expect(calls[1].user).not.toContain("撞击");
  });

  it("retries one transient transport failure but not arbitrary validation errors", async () => {
    let attempts = 0;
    const result = await callVideoLlmWithSafetyRetry(async () => {
      attempts += 1;
      if (attempts === 1) throw new Error("Request failed. Please try again later.");
      return "# 视频剧本\n\n恢复成功";
    }, "SYSTEM", "INPUT");
    expect(result).toContain("恢复成功");
    expect(attempts).toBe(2);
    let gatewayAttempts = 0;
    await callVideoLlmWithSafetyRetry(async () => {
      gatewayAttempts += 1;
      if (gatewayAttempts === 1) throw new Error("524 unknown status code");
      return "恢复";
    }, "SYSTEM", "INPUT");
    expect(gatewayAttempts).toBe(2);
    await expect(callVideoLlmWithSafetyRetry(async () => { throw new Error("invalid model"); }, "SYSTEM", "INPUT")).rejects.toThrow("invalid model");
  });
});
