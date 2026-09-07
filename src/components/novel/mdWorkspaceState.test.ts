// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from "vitest";
import { selectRenderPages, syncDraftBlock, pageBlock } from "./mdWorkspaceState";
import type { MdWorkspace } from "../../lib/comic/markdownApi";
const scope = { projectId: "p", novelWorkId: "w", chapterId: "c" };
function workspace(numbers: number[]): MdWorkspace { return { sourceContent: "正文", sourceRevisionId: "r", documents: numbers.map((pageNo) => ({ id: `d${pageNo}`, pageNo, kind: "page_prompt", markdown: "完整Prompt", revision: 2, stale: false, issues: [], updatedAt: 1 })), jobs: [], images: [], textReady: true, imageReady: true }; }
beforeEach(() => localStorage.clear());
describe("explicit page ranges", () => {
  it("uses content hashes across metadata revisions and falls back for legacy images", () => {
    const data = workspace([1, 2, 3]);
    data.documents.forEach((doc) => { doc.contentHash = "same-content"; });
    data.images = [1, 2, 3].map((pageNo) => ({ id: `i${pageNo}`, documentId: `d${pageNo}`, documentRevision: pageNo === 3 ? 2 : 1, contentHash: pageNo === 1 ? "same-content" : pageNo === 2 ? "changed-content" : undefined, pageNo, path: "/image", promptInjection: "A", stale: false, createdAt: 1 }));
    expect(selectRenderPages(scope, data, "remaining", "A").documents.map((doc) => doc.pageNo)).toEqual([2]);
    expect(selectRenderPages(scope, data, "remaining", "B").documents).toHaveLength(3);
    data.images[0].fileAvailable = false;
    expect(selectRenderPages(scope, data, "remaining", "A").documents.map((doc) => doc.pageNo)).toEqual([1, 2]);
  });
  it("excludes obsolete pages but never overlooks planned missing trailing pages", () => {
    const data = workspace([1, 2, 3]); data.documents[2].outOfPlan = true;
    expect(selectRenderPages(scope, data, "first_three", "").documents.map((doc) => doc.pageNo)).toEqual([1, 2]);
    expect(pageBlock(scope, [data.documents[2]])).toContain("不在当前分镜");
    data.syncPlan = { fingerprint: "p", targets: [], missingPageNos: [3], obsoletePageNos: [], blockedReason: null };
    expect(selectRenderPages(scope, data, "first_three", "").reason).toContain("缺少第3页");
  });
  it("blocks sync for unsaved target and missing-page drafts while ignoring equal persisted drafts", () => {
    const data = workspace([1]);
    data.syncPlan = { fingerprint: "p", targets: [{ documentId: "d1", revision: 2, kind: "page_prompt", pageNo: 1, reasons: ["人物变化"] }], missingPageNos: [2], obsoletePageNos: [], blockedReason: null };
    localStorage.setItem("comic-md:draft:p:w:c:page_prompt:1", JSON.stringify({ markdown: "完整Prompt", expectedRevision: 2 }));
    expect(syncDraftBlock(scope, data)).toBeNull();
    localStorage.setItem("comic-md:draft:p:w:c:page_prompt:2", JSON.stringify({ markdown: "未完成页", expectedRevision: null }));
    expect(syncDraftBlock(scope, data)).toContain("待补的第2页有未保存草稿");
    localStorage.setItem("comic-md:draft:p:w:c:page_prompt:1", JSON.stringify({ markdown: "改稿", expectedRevision: 2 }));
    expect(syncDraftBlock(scope, data)).toContain("第1页 Prompt有未保存修改");
  });
  it("blocks sync when a missing page can make a later saved page update over an unsaved draft", () => {
    const data = workspace([1, 3]);
    data.syncPlan = { fingerprint: "p", targets: [], missingPageNos: [2], obsoletePageNos: [], blockedReason: null };
    localStorage.setItem("comic-md:draft:p:w:c:page_prompt:3", JSON.stringify({ markdown: "第3页未保存改稿", expectedRevision: 2 }));
    expect(syncDraftBlock(scope, data)).toContain("第3页 Prompt 可能被本次联动更新");
  });
  it("selects actual first page and first three, allowing a two-page chapter", () => {
    expect(selectRenderPages(scope, workspace([1, 2, 3, 4]), "first", "").documents.map((doc) => doc.pageNo)).toEqual([1]);
    expect(selectRenderPages(scope, workspace([1, 2, 3, 4]), "first_three", "").documents.map((doc) => doc.pageNo)).toEqual([1, 2, 3]);
    expect(selectRenderPages(scope, workspace([1, 2]), "first_three", "").documents.map((doc) => doc.pageNo)).toEqual([1, 2]);
  });
  it("does not skip missing, invalid or dirty pages", () => {
    expect(selectRenderPages(scope, workspace([2, 3]), "first", "").reason).toContain("第1页");
    expect(selectRenderPages(scope, workspace([1, 3, 4]), "first_three", "").reason).toContain("第2页");
    const data = workspace([1, 2]); data.documents[1].issues = ["人物锚点未补齐"];
    expect(selectRenderPages(scope, data, "first_three", "").reason).toContain("人物锚点");
    data.documents[1].issues = []; localStorage.setItem("comic-md:draft:p:w:c:page_prompt:2", JSON.stringify({ markdown: "新稿", expectedRevision: 2 }));
    expect(selectRenderPages(scope, data, "remaining", "").reason).toContain("第2页有未保存");
  });
  it("counts remaining using actual prompt revision and effective injection, including reverting to an earlier injection", () => {
    const data = workspace([1, 2, 3]);
    data.renderOptions = { promptInjection: "B", revision: 4 };
    data.images = [{ id: "i1", documentId: "d1", documentRevision: 2, pageNo: 1, path: "/i1", promptInjection: "A", stale: true, createdAt: 1 }, { id: "i2", documentId: "d2", documentRevision: 1, pageNo: 2, path: "/i2", promptInjection: "A", stale: true, createdAt: 1 }];
    expect(selectRenderPages(scope, data, "remaining", "A").documents.map((doc) => doc.pageNo)).toEqual([2, 3]);
    expect(selectRenderPages(scope, data, "remaining", "B").documents.map((doc) => doc.pageNo)).toEqual([1, 2, 3]);
    data.workVisualProfile = { constitutionMarkdown: "新画风", revision: 2, references: [] };
    data.images[0].visualProfileRevision = 1;
    expect(selectRenderPages(scope, data, "remaining", "A").documents.map((doc) => doc.pageNo)).toEqual([1, 2, 3]);
    data.documents[0].stale = true;
    expect(selectRenderPages(scope, data, "remaining", "A").reason).toContain("第1页需要更新");
  });
});
