import { describe, expect, it } from "vitest";
import assetsPageSource from "./AssetsPage.tsx?raw";

describe("AssetsPage workspace isolation", () => {
  it("only exposes documents plus the active workspace asset kind", () => {
    expect(assetsPageSource).toContain('const allowedViews: AssetView[] = mode === "image" ? ["text", "image"] : ["text", "video"];');
    expect(assetsPageSource).toContain("{allowedViews.map((kind) => (");
    expect(assetsPageSource).not.toContain('{(["text", "image", "video"] as AssetView[]).map((kind) => (');
  });

  it("uses explicit image/video asset labels and closes prior-workspace asset entry points", () => {
    expect(assetsPageSource).toContain('kind === "image" ? "图像资产" : "视频资产"');
    expect(assetsPageSource).toContain('view === "image" ? "图像资产" : "视频资产"');
    expect(assetsPageSource).toContain("setMenu(null);");
    expect(assetsPageSource).toContain("setPreview(null);");
  });

  it("only loads assets matching the current workspace and keeps image references image-only", () => {
    expect(assetsPageSource).toContain("if (asset.asset.kind !== mode) return;");
    expect(assetsPageSource).toContain('const useImageReference = mode === "image" ? onUseReference : undefined;');
    expect(assetsPageSource).toContain("onLoadAsset={openAssetInWorkspace}");
    expect(assetsPageSource).toContain("onUseReference={useImageReference}");
  });
});
