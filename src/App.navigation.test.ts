import { describe, expect, it } from "vitest";
import appSource from "./App.tsx?raw";

describe("App primary navigation", () => {
  it("routes the primary novel-comic navigation to the results-first page", () => {
    expect(appSource).toContain('setTab("comic")');
    expect(appSource).toContain("小说漫画");
    expect(appSource).toContain("const NovelComicPage");
    expect(appSource).toContain("<NovelComicPage />");
    expect(appSource).not.toContain('{tab === "comic" && <ComicWorkbench />');
  });

  it("exposes one general short-drama agent workspace", () => {
    expect(appSource).toContain("Markdown 视频分镜");
    expect(appSource).toContain("短剧 Agent");
    expect(appSource).toContain("VideoMarkdownWorkspace");
  });

  it("uses explicit image and video navigation without a duplicate mode toggle", () => {
    expect(appSource).toContain('<ImageIcon size={13} /> 图像生成');
    expect(appSource).toContain('<Clapperboard size={13} /> 视频生成');
    expect(appSource).toContain('setMode("image"); setTab("generate")');
    expect(appSource).toContain('setMode("video"); setTab("generate")');
    expect(appSource).not.toContain("Mode toggle (right)");
  });

  it("wires the status bar documentation entries to the shared dialog", () => {
    expect(appSource).toContain('setDocsView("guide")');
    expect(appSource).toContain('setDocsView("changelog")');
    expect(appSource).toContain("<AppDocsDialog");
  });

  it("scopes the header asset count to the active project and active workspace", () => {
    expect(appSource).toContain('const allowedAssetKinds = mode === "image" ? ["text", "image"] : ["text", "video"];');
    expect(appSource).toContain("belongsToActiveProject && allowedAssetKinds.includes(asset.asset.kind)");
  });

  it("opens media in its matching workspace and clears video references on project changes", () => {
    expect(appSource).toContain('if (asset.asset.kind === "video")');
    expect(appSource).toContain('} else if (asset.asset.kind === "image")');
    expect(appSource).toContain("images: []");
    expect(appSource).toContain("videos: []");
    expect(appSource).toContain("audios: []");
    expect(appSource).toContain('mode: "text"');
    expect(appSource).toContain("clearProjectScopedGenerationState();");
  });

  it("hands Markdown storyboard shots to the isolated video workspace", () => {
    expect(appSource).toContain("buildReviewedVideoHandoff(shots");
    expect(appSource).toContain("useVideoStore.getState().set(handoff)");
    expect(appSource).toContain("approvedResolution: string");
    expect(appSource).toContain('setMode("video")');
    expect(appSource).toContain("onSendToVideo={sendStoryboardToVideo}");
  });
});
