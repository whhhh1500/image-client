// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import AssetImportPicker from "./AssetImportPicker";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import type { ImportEntry } from "../lib/assetImport";

const project = (id: string) => ({ id, name: id, description: "", storyStyle: "", artStyle: "", aspectRatio: "16:9", imageModel: "image", imageQuality: "high", videoModel: "video", videoResolution: "720p" });
function text(id: string, projectId: string, body?: string): LibAsset {
  return { asset: { id, kind: "text", path: `C:/${id}.md` }, source: id, projectId, params: body ? { text: body } : {}, createdAt: 1 };
}

afterEach(() => { cleanup(); useLibraryStore.setState({ assets: [], tasks: [] }); });

describe("AssetImportPicker", () => {
  it("previews a selected comic prompt group in authored page order and cancellation has no side effect", () => {
    const apply = vi.fn();
    const one: ImportEntry = { entryType: "canonical_comic", readonly: true, sourceUri: "comic://two", projectId: "p", kind: "text", title: "第2页提示词", text: "第二页", novelWorkId: "work", novelChapterId: "chapter", documentKind: "page_prompt", pageNo: 2, createdAt: 2 };
    const two: ImportEntry = { ...one, sourceUri: "comic://one", title: "第1页提示词", text: "第一页", pageNo: 1, createdAt: 1 };
    useProjectStore.setState({ projects: [project("p")], activeId: "p" });
    render(<AssetImportPicker open onClose={vi.fn()} actions={["merge_prompt"]} kinds={["text"]} entries={[one, two]} onApply={apply} />);
    fireEvent.change(screen.getByLabelText("资产组筛选"), { target: { value: "comic_chapter:work::chapter" } });
    fireEvent.click(screen.getByRole("button", { name: "图片版本" }));
    expect(screen.getByText("当前项目没有可导入的匹配资产。")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "页面提示词" }));
    expect(screen.getByText("第1页提示词")).toBeTruthy();
    fireEvent.click(screen.getByText("第2页提示词"));
    fireEvent.click(screen.getByText("第1页提示词"));
    expect(screen.getByLabelText("导入预览").textContent).toMatch(/1\. 第1页提示词[\s\S]*2\. 第2页提示词/);
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(apply).not.toHaveBeenCalled();
  });

  it("drops stale selection on a project change and applies only entries with actual prompt text", async () => {
    const apply = vi.fn();
    const close = vi.fn();
    const usable = text("可用", "p", "真实正文");
    const missing = text("缺失", "p");
    useProjectStore.setState({ projects: [project("p"), project("other")], activeId: "p" });
    useLibraryStore.setState({ assets: [usable, missing], tasks: [] });
    const rendered = render(<AssetImportPicker open onClose={close} actions={["merge_prompt"]} kinds={["text"]} onApply={apply} />);
    fireEvent.click(screen.getByText("可用"));
    fireEvent.click(screen.getByText("缺失"));
    expect(screen.getByText(/缺少可导入的已保存正文/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "确认应用" }));
    await waitFor(() => expect(apply).toHaveBeenCalledWith(expect.objectContaining({ entries: [expect.objectContaining({ asset: expect.objectContaining({ source: "可用" }) })] })));
    rendered.rerender(<AssetImportPicker open onClose={close} actions={["merge_prompt"]} kinds={["text"]} onApply={apply} initialEntries={[{ entryType: "library_asset", asset: usable }]} />);
    useProjectStore.setState({ activeId: "other" });
    rendered.rerender(<AssetImportPicker open onClose={close} actions={["merge_prompt"]} kinds={["text"]} onApply={apply} initialEntries={[{ entryType: "library_asset", asset: usable }]} />);
    expect(screen.queryByText("可用")).toBeNull();
  });
});
