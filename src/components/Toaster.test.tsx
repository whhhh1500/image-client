// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import Toaster from "./Toaster";
import { toast, useToastStore } from "../store/useToastStore";

describe("Toaster", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useToastStore.setState({ toasts: [] });
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("replaces a progress toast with its result, which then fades", () => {
    render(<Toaster />);
    let id = 0;
    act(() => { id = toast("progress", "正在压缩…"); });
    expect(screen.getByRole("status").textContent).toBe("正在压缩…");
    expect(screen.queryByRole("button", { name: "关闭提示" })).toBeNull();

    act(() => { toast("success", "压缩完成", id); });
    expect(screen.getAllByRole("status")).toHaveLength(1);
    expect(screen.getByRole("status").textContent).toBe("压缩完成");
    act(() => { vi.advanceTimersByTime(6000); });
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("keeps an error until it is dismissed and shows at most four toasts", () => {
    render(<Toaster />);
    act(() => { toast("error", "转换失败：磁盘已满"); });
    act(() => { vi.advanceTimersByTime(60_000); });
    expect(screen.getByRole("alert").textContent).toBe("转换失败：磁盘已满");
    fireEvent.click(screen.getByRole("button", { name: "关闭提示" }));
    expect(screen.queryByRole("alert")).toBeNull();

    act(() => { for (let n = 1; n <= 6; n += 1) toast("info", `提示 ${n}`); });
    expect(screen.getAllByRole("status").map((node) => node.textContent)).toEqual(["提示 3", "提示 4", "提示 5", "提示 6"]);
  });
});
