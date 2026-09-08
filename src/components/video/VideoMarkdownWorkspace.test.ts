import { describe, expect, it } from "vitest";
import source from "./VideoMarkdownWorkspace.tsx?raw";

describe("VideoMarkdownWorkspace", () => {
  it("uses a staged Markdown workflow for novels and ideas", () => {
    for (const label of ["原始资料", "改编规划", "剧本", "视频锚点", "视频分镜", "质检", "视频结果"]) expect(source).toContain(label);
    expect(source).toContain("小说章节快照");
    expect(source).toContain("脑洞 / 梗概");
  });

  it("generates stages from saved dependencies and hands shots to video generation", () => {
    expect(source).toContain("dependencyViews(view)");
    expect(source).toContain("parseStoryboardShots");
    expect(source).toContain("onSendToVideo");
  });

  it("keeps generation editable while optimization directly saves the reviewed downstream chain", () => {
    expect(source).toContain("AI 优化");
    expect(source).toContain("全部联动草稿已通过审查");
    expect(source).toContain("savedByStage");
    expect(source).toContain("changeType: \"ai_optimized\"");
    expect(source).toContain("保存迁移版本");
    expect(source).toContain("放弃草稿并恢复已保存内容");
  });

  it("carries confirmed anchors from previous chapters without mixing workflows", () => {
    expect(source).toContain("previousChapterAnchorAssets");
    expect(source).toContain("videoWorkflowId");
    expect(source).toContain("固定锚定跨章继承");
    expect(source).toContain("本章保存完整快照");
  });
});
