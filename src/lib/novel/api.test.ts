import { beforeEach, describe, expect, it, vi } from "vitest";
const invoke = vi.hoisted(() => vi.fn());
vi.mock("../logger", () => ({ loggedInvoke: invoke }));
import { novelWorkList, novelWorkCreate, novelWorkGet, novelChapterRevisionCreate } from "./api";
describe("novel source API", () => {
  beforeEach(() => invoke.mockReset());
  it("reads novels and chapters through project-scoped envelopes", async () => {
    invoke.mockResolvedValueOnce({ items: [{ id: "w", title: "小说" }] });
    await expect(novelWorkList("p")).resolves.toEqual([{ id: "w", title: "小说" }]);
    expect(invoke).toHaveBeenLastCalledWith("novel_work_list", { input: { projectId: "p", includeArchived: true } });
    await novelWorkGet({ projectId: "p", novelWorkId: "w" });
    expect(invoke).toHaveBeenLastCalledWith("novel_work_get", { input: { projectId: "p", novelWorkId: "w" } });
  });
  it("creates novels and saves exact source/title without invoking any production workflow", async () => {
    await novelWorkCreate({ projectId: "p", title: "新小说", idempotencyKey: "new-work" });
    const source = { projectId: "p", novelWorkId: "w", chapterId: "c", chapterNo: 1, title: "新章名", content: "正文\n保留换行", idempotencyKey: "source" };
    await novelChapterRevisionCreate(source);
    expect(invoke.mock.calls).toEqual([
      ["novel_work_create", { input: { projectId: "p", title: "新小说", idempotencyKey: "new-work" } }],
      ["novel_chapter_revision_create", { input: source }],
    ]);
  });
});
