import { describe, expect, it } from "vitest";
import { imageSizeForRatio, projectContext } from "./projectProfile";

describe("project profile", () => {
  it("maps project ratios to supported image sizes", () => {
    expect(imageSizeForRatio("16:9")).toBe("1280x720 (16:9)");
    expect(imageSizeForRatio("9:16")).toBe("720x1280 (9:16)");
    expect(imageSizeForRatio("unknown")).toBe("1024x1024 (1:1)");
  });

  it("builds concise Agent context", () => {
    const text = projectContext({
      id: "p1",
      name: "测试项目",
      description: "一部悬疑短剧",
      storyStyle: "悬疑惊悚",
      artStyle: "电影写实",
      aspectRatio: "16:9",
      imageModel: "image-model",
      imageQuality: "high",
      videoModel: "video-model",
      videoResolution: "720p",
    });
    expect(text).toContain("项目简介：一部悬疑短剧");
    expect(text).toContain("目标画幅：16:9");
  });
});
