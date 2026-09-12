import { describe, expect, it } from "vitest";
import { isGrokImageModel } from "./models";

describe("isGrokImageModel", () => {
  it("recognizes every grok image model id shape", () => {
    expect(isGrokImageModel("grok-imagine-image")).toBe(true);
    expect(isGrokImageModel("grok-3-image")).toBe(true);
    expect(isGrokImageModel("grok3-image")).toBe(true);
    expect(isGrokImageModel("xai/grok-2-image")).toBe(true);
    expect(isGrokImageModel("  Grok-2-Image-1212  ")).toBe(true);
  });

  it("leaves the OpenAI-compatible models on the size contract", () => {
    expect(isGrokImageModel("gpt-image-2")).toBe(false);
    expect(isGrokImageModel("gpt-image-2.5-sunburst")).toBe(false);
    expect(isGrokImageModel("gemini-3-pro-image")).toBe(false);
    expect(isGrokImageModel("nana-banana-pro")).toBe(false);
    // Merely containing the letters must not switch the parameter contract.
    expect(isGrokImageModel("grokking-image")).toBe(false);
    expect(isGrokImageModel("")).toBe(false);
  });
});
