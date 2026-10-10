import { afterEach, describe, expect, it } from "vitest";
import { bindFormsToProject, resetProjectDrafts, restoreProjectDrafts, stashProjectDrafts } from "./projectDrafts";
import { useGenerationStore } from "./useGenerationStore";

const template = { id: "tpl", kind: "template" as const, title: "电影海报", prompt: "模板提示词", category: "poster", styles: [], scenes: [], tags: [], image: "" };

/** What App does on a project switch: stash, clear the project-scoped fields, restore the target. */
function switchForms(projectId: string) {
  stashProjectDrafts();
  useGenerationStore.getState().set({ prompt: "", referencePath: "", references: [], importedSources: [] });
  return restoreProjectDrafts(projectId);
}

afterEach(() => {
  resetProjectDrafts();
  useGenerationStore.getState().reset();
});

describe("project drafts", () => {
  it("keeps the prompt template with its project's prompt", () => {
    bindFormsToProject("p1");
    useGenerationStore.getState().set({ prompt: "项目一的模板提示词", promptTemplate: template });

    expect(switchForms("p2")).toBe(false);
    // The previous project's template must not label the next project's prompt.
    expect(useGenerationStore.getState()).toMatchObject({ prompt: "", promptTemplate: null });
    useGenerationStore.getState().set({ prompt: "项目二手写提示词" });

    expect(switchForms("p1")).toBe(true);
    expect(useGenerationStore.getState()).toMatchObject({ prompt: "项目一的模板提示词", promptTemplate: template });

    expect(switchForms("p2")).toBe(true);
    expect(useGenerationStore.getState()).toMatchObject({ prompt: "项目二手写提示词", promptTemplate: null });
  });
});
