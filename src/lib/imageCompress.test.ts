import { describe, expect, it } from "vitest";
import { generationParamsSummary, isCompressedAsset, latestDisplayPath } from "./imageCompress";

describe("image compression helpers", () => {
  it("detects compressed filenames and keeps originals", () => {
    expect(isCompressedAsset({
      asset: { id: "1", kind: "image", path: "C:/out/hero-png-20260901010101.png" },
      source: "转换为 PNG",
      createdAt: 1,
      params: { convertFormat: "png" },
    })).toBe(true);
    expect(isCompressedAsset({
      asset: { id: "2", kind: "image", path: "C:/out/hero.jpg" },
      source: "文生图",
      createdAt: 1,
    })).toBe(false);
  });

  it("summarizes generation params and prefers latest display path", () => {
    expect(generationParamsSummary({ model: "gpt-image-2", size: "1024x1024 (1:1)", quality: "high" })).toContain("模型 gpt-image-2");
    // A batch mentions its size; a single-image run stays quiet about it.
    expect(generationParamsSummary({ model: "gpt-image-2", count: 4 })).toContain("4 张");
    expect(generationParamsSummary({ model: "gpt-image-2", count: 1 })).not.toContain("张");
    expect(latestDisplayPath({
      path: "C:/out/hero.jpg",
      directory: "C:/out",
      fileName: "hero.jpg",
      bytes: 1000,
      kb: 1,
      displayPath: "C:/out/hero-q60-20260901010101.jpg",
      variants: [],
    })).toBe("C:/out/hero-q60-20260901010101.jpg");
  });
});
