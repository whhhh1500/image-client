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

  it("does not expose the legacy five-minute drama studio as an independent page", () => {
    expect(appSource).not.toContain("五分钟大赛剧本");
    expect(appSource).not.toContain('setTab("drama")');
    expect(appSource).not.toContain("const DramaStudio");
    expect(appSource).not.toMatch(/\btab === "drama"/u);
  });

  it("wires the status bar documentation entries to the shared dialog", () => {
    expect(appSource).toContain('setDocsView("guide")');
    expect(appSource).toContain('setDocsView("changelog")');
    expect(appSource).toContain("<AppDocsDialog");
  });
});
