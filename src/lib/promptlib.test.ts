import { describe, expect, it } from "vitest";
import {
  applyPlaceholders,
  catalog,
  categoryLabel,
  extractPlaceholders,
  findEntry,
  searchEntries,
} from "./promptlib";

describe("promptlib catalog", () => {
  it("loads bundled cases and industrial templates", () => {
    expect(catalog.cases.length).toBe(541);
    expect(catalog.templates.length).toBe(22);
    expect(catalog.imageBaseUrl).toContain("r2.dev/img-case-assets");
    expect(findEntry("tpl:ui-screenshot-system")?.prompt).toContain("[产品类型]");
    expect(findEntry("tpl:conceptual-typography-poster")?.prompt).toContain("概念字体海报");
    expect(findEntry("tpl:ui-screenshot-system")?.jsonPrompt).toContain("类型");
    expect(findEntry("case:544")?.prompt).toContain("[FRUIT]");
  });

  it("extracts and fills placeholders without touching unused tokens", () => {
    const prompt = "Feature [FRUIT] and a [PART / SLICE] on a [COLOR] card.";
    expect(extractPlaceholders(prompt)).toEqual(["FRUIT", "PART / SLICE", "COLOR"]);
    expect(applyPlaceholders(prompt, { FRUIT: "apple", COLOR: "blue" })).toBe(
      "Feature apple and a [PART / SLICE] on a blue card.",
    );
  });

  it("searches Chinese labels, prompt text, and modified-only overrides", () => {
    const ui = catalog.templates.find((item) => item.id === "tpl:ui-screenshot-system")!;
    const hits = searchEntries(catalog.templates, {}, { query: "界面", kind: "template" });
    expect(hits.some((item) => item.id === ui.id)).toBe(true);
    expect(categoryLabel(ui.category)).toBe("UI 与界面");

    const edited = searchEntries(catalog.cases, { "case:544": { prompt: "custom banana card", updatedAt: 1 } }, {
      query: "banana",
      modifiedOnly: true,
    });
    expect(edited.map((item) => item.id)).toEqual(["case:544"]);
  });

  it("maps 动漫 to anime so English prompts are findable", () => {
    const hits = searchEntries(catalog.cases, {}, { query: "动漫" });
    expect(hits.length).toBeGreaterThan(20);
    expect(hits.some((item) => item.title.includes("动漫") || (item.prompt ?? "").toLowerCase().includes("anime"))).toBe(true);
  });

  it("requires every search token to match, while expanding synonyms per token", () => {
    const anime = searchEntries(catalog.cases, {}, { query: "动漫" });
    const narrowed = searchEntries(catalog.cases, {}, { query: "动漫 海报" });
    expect(anime.length).toBeGreaterThan(0);
    expect(narrowed.length).toBeGreaterThan(0);
    expect(narrowed.length).toBeLessThanOrEqual(anime.length);
  });
});
