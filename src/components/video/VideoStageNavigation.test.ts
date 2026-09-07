// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import VideoStageNavigation, { videoResultStageStatus } from "./VideoStageNavigation";

afterEach(cleanup);

describe("video result stage progress", () => {
  it("does not call a partially generated storyboard complete", () => {
    expect(videoResultStageStatus(12, 1, 0, true)).toBe("partial");
    expect(videoResultStageStatus(12, 12, 0, true)).toBe("current");
    expect(videoResultStageStatus(12, 0, 1, true)).toBe("partial");
    expect(videoResultStageStatus(12, 12, 0, false)).toBe("blocked");
  });
});

describe("video stage tabs", () => {
  const items = [
    { id: "source", label: "原始资料", status: "current" as const },
    { id: "script", label: "剧本", status: "not_started" as const },
    { id: "storyboard", label: "视频分镜", status: "blocked" as const },
  ];

  it("uses tab semantics and moves both selection request and focus with keyboard navigation", () => {
    const onSelect = vi.fn();
    render(createElement(VideoStageNavigation, { idPrefix: "video-workspace", items, current: "source", onSelect }));

    const tablist = screen.getByRole("tablist", { name: "短剧生产阶段" });
    const source = screen.getByRole("tab", { name: /原始资料/ });
    const script = screen.getByRole("tab", { name: /剧本/ });
    const storyboard = screen.getByRole("tab", { name: /视频分镜/ });
    expect(tablist.getAttribute("aria-orientation")).toBe("horizontal");
    expect(source.getAttribute("aria-selected")).toBe("true");
    expect(source.getAttribute("tabindex")).toBe("0");
    expect(source.getAttribute("aria-controls")).toBe("video-workspace-panel-source");
    expect(script.getAttribute("aria-selected")).toBe("false");
    expect(script.getAttribute("tabindex")).toBe("-1");

    source.focus();
    fireEvent.keyDown(source, { key: "ArrowRight" });
    expect(onSelect).toHaveBeenLastCalledWith("script");
    expect(document.activeElement).toBe(script);

    fireEvent.keyDown(script, { key: "End" });
    expect(onSelect).toHaveBeenLastCalledWith("storyboard");
    expect(document.activeElement).toBe(storyboard);

    fireEvent.keyDown(storyboard, { key: "Home" });
    expect(onSelect).toHaveBeenLastCalledWith("source");
    expect(document.activeElement).toBe(source);

    fireEvent.keyDown(source, { key: "ArrowLeft" });
    expect(onSelect).toHaveBeenLastCalledWith("storyboard");
    expect(document.activeElement).toBe(storyboard);
  });
});
