// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
vi.mock("../components/novel/MarkdownComicWorkspace", () => ({ default: () => <section aria-label="Markdown 工作区" /> }));
import NovelComicPage from "./NovelComicPage";
afterEach(cleanup);
describe("NovelComicPage", () => {
  it("renders only the Markdown workspace without any old workflow controls", () => {
    render(<NovelComicPage />);
    expect(screen.getByLabelText("Markdown 工作区")).toBeTruthy();
    expect(screen.queryByText(/旧版资料|历史漫画工作台|小说分析与产物|返回小说漫画/)).toBeNull();
    expect(screen.queryAllByRole("button")).toHaveLength(0);
  });
});
