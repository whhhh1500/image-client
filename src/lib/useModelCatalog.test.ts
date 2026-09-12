// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import { useModelCatalog } from "./useModelCatalog";
import { fetchModels } from "./ipc";

vi.mock("./ipc", () => ({ fetchModels: vi.fn() }));
vi.mock("./logger", () => ({ logEvent: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.mocked(fetchModels).mockReset();
});

describe("useModelCatalog", () => {
  it("replaces the suggestions with the catalog of the credentials it was given", async () => {
    vi.mocked(fetchModels).mockResolvedValue(["brand-new-image", "gpt-image-4"]);
    const { result } = renderHook(() => useModelCatalog("image", ["gpt-image-2"]));
    expect(result.current.options).toEqual(["gpt-image-2"]);

    await act(async () => {
      await result.current.refresh({ url: "https://gw.example/v1/images/generations", key: "sk-typed" });
    });

    expect(vi.mocked(fetchModels)).toHaveBeenCalledWith({
      url: "https://gw.example/v1/images/generations",
      key: "sk-typed",
      kind: "image",
    });
    expect(result.current.options).toEqual(["brand-new-image", "gpt-image-4"]);
    expect(result.current.message).toContain("已获取 2 个模型");
    expect(result.current.error).toBe(false);
    expect(result.current.loading).toBe(false);
  });

  it("asks the backend for the saved credentials when the caller holds none", async () => {
    vi.mocked(fetchModels).mockResolvedValue(["kling-video-v3"]);
    const { result } = renderHook(() => useModelCatalog("video", []));

    await act(async () => {
      await result.current.refresh();
    });

    expect(vi.mocked(fetchModels)).toHaveBeenCalledWith({ url: "", key: "", kind: "video" });
    expect(result.current.options).toEqual(["kling-video-v3"]);
  });

  it("keeps the previous suggestions and reports why the fetch failed", async () => {
    vi.mocked(fetchModels).mockRejectedValue("模型目录返回 401 Unauthorized");
    const { result } = renderHook(() => useModelCatalog("llm", ["gemini-3.7-flash"]));

    await act(async () => {
      await result.current.refresh();
    });

    expect(result.current.options).toEqual(["gemini-3.7-flash"]);
    expect(result.current.error).toBe(true);
    expect(result.current.message).toBe("获取模型失败：模型目录返回 401 Unauthorized");
  });

  it("clears the previous result message on demand", async () => {
    vi.mocked(fetchModels).mockResolvedValue(["gpt-image-2"]);
    const { result } = renderHook(() => useModelCatalog("image", []));
    await act(async () => {
      await result.current.refresh();
    });
    expect(result.current.message).not.toBeNull();

    act(() => result.current.clearMessage());
    expect(result.current.message).toBeNull();
    expect(result.current.error).toBe(false);
  });
});
