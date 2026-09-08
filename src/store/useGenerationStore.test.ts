import { afterEach, describe, expect, it } from "vitest";
import { useGenerationStore } from "./useGenerationStore";

afterEach(() => useGenerationStore.getState().reset());

describe("useGenerationStore history compatibility", () => {
  it("clears current multi-reference inputs when an older history record has none", () => {
    useGenerationStore.getState().set({
      referencePath: "D:/current.png",
      references: [{ path: "D:/current.png", role: "style" }],
      importedSources: [{ action: "reference", assetIds: ["current-reference"], sourceMaterials: [] }],
    });

    useGenerationStore.getState().load({ prompt: "旧历史提示词" });

    expect(useGenerationStore.getState()).toMatchObject({
      prompt: "旧历史提示词",
      referencePath: "",
      references: [],
      importedSources: [],
    });
  });

  it("reset clears every multi-reference input", () => {
    useGenerationStore.getState().set({
      referencePath: "D:/current.png",
      references: [{ path: "D:/current.png", role: "style" }],
      importedSources: [{ action: "reference", assetIds: ["current-reference"], sourceMaterials: [] }],
    });

    useGenerationStore.getState().reset();

    expect(useGenerationStore.getState()).toMatchObject({
      referencePath: "",
      references: [],
      importedSources: [],
    });
  });
});
