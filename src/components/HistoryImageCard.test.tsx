// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { LibAsset } from "../store/useLibraryStore";

const inspect = vi.hoisted(() => vi.fn());
vi.mock("../lib/desktop", () => ({ fileSrc: (path: string) => path }));
vi.mock("../lib/logger", () => ({ logEvent: vi.fn() }));
vi.mock("../lib/imageCompress", () => ({
  inspectImageCached: inspect,
  subscribeImageInfo: () => () => {},
  latestDisplayPath: (info: { displayPath?: string; path: string }) => info.displayPath || info.path,
  generationParamsSummary: () => "",
}));

import HistoryImageCard from "./HistoryImageCard";

const asset: LibAsset = { asset: { id: "a1", kind: "image", path: "C:/a.png" }, source: "历史图", createdAt: 1 };

afterEach(() => {
  cleanup();
  inspect.mockReset();
  vi.unstubAllGlobals();
});

describe("HistoryImageCard", () => {
  it("loads the image lazily and inspects at once without IntersectionObserver", () => {
    inspect.mockReturnValue(new Promise(() => {}));
    render(<HistoryImageCard asset={asset} onOpen={vi.fn()} onMenu={vi.fn()} />);
    const image = screen.getByAltText("历史图");
    expect(image.getAttribute("loading")).toBe("lazy");
    expect(image.getAttribute("decoding")).toBe("async");
    expect(inspect).toHaveBeenCalledWith("C:/a.png");
  });

  it("opens from the keyboard like a click", async () => {
    inspect.mockReturnValue(new Promise(() => {}));
    const onOpen = vi.fn();
    const user = userEvent.setup();
    render(<HistoryImageCard asset={asset} onOpen={onOpen} onMenu={vi.fn()} />);

    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "历史图" }));
    await user.keyboard("{Enter}");
    await user.keyboard(" ");
    expect(onOpen).toHaveBeenCalledTimes(2);
    expect(onOpen).toHaveBeenCalledWith("C:/a.png");
  });

  it("waits until the card scrolls into view before inspecting the file", () => {
    inspect.mockReturnValue(new Promise(() => {}));
    let notify: (entries: Array<{ isIntersecting: boolean }>) => void = () => {};
    const disconnect = vi.fn();
    vi.stubGlobal("IntersectionObserver", class {
      constructor(callback: typeof notify) { notify = callback; }
      observe() {}
      disconnect = disconnect;
    });

    render(<HistoryImageCard asset={asset} onOpen={vi.fn()} onMenu={vi.fn()} />);
    expect(inspect).not.toHaveBeenCalled();

    act(() => notify([{ isIntersecting: false }]));
    expect(inspect).not.toHaveBeenCalled();

    act(() => notify([{ isIntersecting: true }]));
    expect(inspect).toHaveBeenCalledTimes(1);
    expect(disconnect).toHaveBeenCalled();
  });
  it("shows its own file in the asset library and the newest variant only where asked", async () => {
    const variants = [
      { path: "C:/a-webp-20261011000000000.webp", fileName: "a-webp-20261011000000000.webp", level: "webp", bytes: 2048, kb: 2, modifiedAt: 2 },
      { path: "C:/a.png", fileName: "a.png", level: "original", bytes: 4096, kb: 4, modifiedAt: 1 },
    ];
    inspect.mockResolvedValue({ path: "C:/a.png", directory: "C:/", fileName: "a.png", bytes: 4096, kb: 4, displayPath: variants[0].path, variants });

    const library = render(<HistoryImageCard asset={asset} onOpen={vi.fn()} onMenu={vi.fn()} />);
    await act(async () => {});
    expect(screen.getByAltText("历史图").getAttribute("src")).toBe("C:/a.png");
    library.unmount();

    const onOpen = vi.fn();
    render(<HistoryImageCard asset={asset} onOpen={onOpen} onMenu={vi.fn()} showLatestVariant />);
    await act(async () => {});
    expect(screen.getByAltText("历史图").getAttribute("src")).toBe(variants[0].path);
    screen.getByRole("button").click();
    expect(onOpen).toHaveBeenCalledWith(variants[0].path);
  });
});
