// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import { resetViewState, useViewState } from "./useViewStore";

describe("useViewState", () => {
  afterEach(() => {
    cleanup();
    resetViewState();
  });

  it("keeps the value across an unmount and starts from initial until set", () => {
    const first = renderHook(() => useViewState("page:filter", "all"));
    expect(first.result.current[0]).toBe("all");
    act(() => first.result.current[1]("video"));
    first.unmount();

    const second = renderHook(() => useViewState("page:filter", "all"));
    expect(second.result.current[0]).toBe("video");
  });

  it("applies updaters to the latest stored value, even after the caller unmounted", () => {
    const view = renderHook(() => useViewState<Record<string, string>>("page:drafts", {}));
    const setDrafts = view.result.current[1];
    act(() => setDrafts((drafts) => ({ ...drafts, a: "one" })));
    view.unmount();

    // A late async result written through a setter captured before the unmount.
    act(() => setDrafts((drafts) => ({ ...drafts, b: "two" })));
    const again = renderHook(() => useViewState<Record<string, string>>("page:drafts", {}));
    expect(again.result.current[0]).toEqual({ a: "one", b: "two" });
  });

  it("keeps keys apart", () => {
    const a = renderHook(() => useViewState("page:1", 1));
    const b = renderHook(() => useViewState("page:2", 1));
    act(() => a.result.current[1]((n) => n + 1));
    expect(a.result.current[0]).toBe(2);
    expect(b.result.current[0]).toBe(1);
  });
});
