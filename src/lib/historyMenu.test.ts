// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { LibAsset } from "../store/useLibraryStore";
import { useToastStore } from "../store/useToastStore";

const mocks = vi.hoisted(() => ({ compress: vi.fn(), convert: vi.fn() }));
vi.mock("./desktop", () => ({ revealItemInDir: vi.fn() }));
vi.mock("./logger", () => ({ logEvent: vi.fn() }));
vi.mock("./imageCompress", () => ({
  compressHistoryImage: mocks.compress,
  convertHistoryImage: mocks.convert,
  formatBytes: (bytes: number) => (bytes < 1024 * 1024 ? `${Math.ceil(bytes / 1024)} KB` : `${(bytes / 1024 / 1024).toFixed(1)} MB`),
}));

import { imageHistoryMenu } from "./historyMenu";

const image = (path: string): LibAsset => ({ asset: { id: path, kind: "image", path }, source: "历史图", createdAt: 1 });
const labels = (path: string) => imageHistoryMenu({ asset: image(path), onPreview: vi.fn() }).map((item) => item.label);
const click = (path: string, label: string) => imageHistoryMenu({ asset: image(path), onPreview: vi.fn() }).find((item) => item.label === label)!.onClick();
const toasts = () => useToastStore.getState().toasts.map(({ tone, text }) => ({ tone, text }));
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("imageHistoryMenu", () => {
  beforeEach(() => useToastStore.setState({ toasts: [] }));
  afterEach(() => vi.clearAllMocks());

  it("offers no lossless JPEG and no conversion to the format the image already has", () => {
    expect(labels("C:/out/a.PNG")).toEqual([
      "放大预览", "无损压缩", "有损压缩 80%", "有损压缩 60%",
      "转换为 JPG", "转换为 WEBP", "转换为 BMP", "转换为 GIF", "复制文件路径", "打开所在文件夹",
    ]);
    const jpeg = labels("C:/out/b.jpeg");
    expect(jpeg).not.toContain("无损压缩");
    expect(jpeg).not.toContain("转换为 JPG");
    expect(jpeg).toContain("转换为 PNG");
  });

  it("starts each group with a separator", () => {
    const items = imageHistoryMenu({ asset: image("C:/a.png"), onPreview: vi.fn(), onReference: vi.fn() });
    expect(items.filter((item) => item.divider).map((item) => item.label)).toEqual(["无损压缩", "转换为 JPG", "复制文件路径"]);
  });

  it("shows progress, then the size change of the written file", async () => {
    let finish!: (value: unknown) => void;
    mocks.compress.mockReturnValue(new Promise((resolve) => { finish = resolve; }));
    click("C:/a.png", "有损压缩 80%");
    expect(toasts()).toEqual([{ tone: "progress", text: "正在有损压缩 80%…" }]);

    finish({ asset: image("C:/a-q80.jpg"), fromBytes: 3 * 1024 * 1024, toBytes: 459 * 1024 });
    await settle();
    expect(toasts()).toEqual([{ tone: "success", text: "有损压缩 80% 完成：3.0 MB → 459 KB（减少 85%），已保存在原图所在文件夹" }]);
  });

  it("names a conversion and says when the image cannot get smaller instead of calling it a failure", async () => {
    mocks.convert.mockResolvedValue({ asset: image("C:/a.webp"), fromBytes: 2048, toBytes: 4096 });
    click("C:/a.png", "转换为 WEBP");
    await settle();
    expect(toasts()).toEqual([{ tone: "success", text: "已转换为 WEBP：2 KB → 4 KB，已保存在原图所在文件夹" }]);

    useToastStore.setState({ toasts: [] });
    mocks.compress.mockRejectedValue("压缩后不会更小（原图 459 KB，压缩后 812 KB），未生成新文件");
    click("C:/a.png", "无损压缩");
    await settle();
    expect(toasts()).toEqual([{ tone: "info", text: "压缩后不会更小（原图 459 KB，压缩后 812 KB），未生成新文件" }]);
  });

  it("reports a failure without an Error: prefix and ignores a second click while running", async () => {
    let fail!: (error: Error) => void;
    mocks.convert.mockReturnValue(new Promise((_resolve, reject) => { fail = reject; }));
    click("C:/a.png", "转换为 GIF");
    click("C:/a.png", "转换为 GIF");
    expect(mocks.convert).toHaveBeenCalledTimes(1);
    expect(toasts().map((item) => item.text)).toEqual(["正在转换为 GIF…", "正在转换为 GIF，请等待完成"]);

    fail(new Error("磁盘已满"));
    await settle();
    expect(toasts()[1]).toEqual({ tone: "error", text: "转换为 GIF 失败：磁盘已满" });
  });
});
