// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { confirmAction } from "../lib/confirm";
import ProjectSettingsPage from "./ProjectSettingsPage";
import type { Project } from "../store/useProjectStore";

vi.mock("../lib/confirm", () => ({ confirmAction: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it.each([false, true])("waits for confirmation and only closes on acceptance (%s)", async (accepted) => {
  let resolveConfirmation!: (value: boolean) => void;
  vi.mocked(confirmAction).mockReturnValue(new Promise<boolean>((resolve) => { resolveConfirmation = resolve; }));
  const onClose = vi.fn();
  const project: Project = {
    id: "test", name: "项目", description: "", storyStyle: "通用短剧", artStyle: "电影写实",
    aspectRatio: "16:9", imageModel: "gpt-image-2", imageQuality: "high", videoModel: "grok-imagine-video",
  };
  render(<ProjectSettingsPage open project={project} onClose={onClose} />);
  fireEvent.change(screen.getByLabelText("项目名称"), { target: { value: "未保存修改" } });
  fireEvent.click(screen.getByRole("button", { name: "取消" }));
  expect(confirmAction).toHaveBeenCalledOnce();
  expect(onClose).not.toHaveBeenCalled();
  await act(async () => { resolveConfirmation(accepted); });
  expect(onClose).toHaveBeenCalledTimes(accepted ? 1 : 0);
  expect((screen.getByLabelText("项目名称") as HTMLInputElement).value).toBe("未保存修改");
});
