import { beforeEach, describe, expect, it, vi } from "vitest";

const { dbSelect, dbExecute } = vi.hoisted(() => ({ dbSelect: vi.fn(), dbExecute: vi.fn() }));

vi.mock("./db", () => ({ dbSelect, dbExecute }));
vi.mock("./logger", () => ({ logEvent: vi.fn() }));

import { updateAssetMetadata } from "./dbWrite";
import type { LibAsset } from "../store/useLibraryStore";

function libAsset(params: Record<string, unknown>): LibAsset {
  return {
    asset: { id: "asset-1", kind: "image", path: "C:/out/asset-1.png" },
    source: "漫画页",
    model: "gpt-image-2",
    projectId: "project-1",
    params,
    createdAt: 1,
  };
}

describe("updateAssetMetadata", () => {
  beforeEach(() => {
    dbSelect.mockReset();
    dbExecute.mockReset();
  });

  it("preserves metadata keys owned by other producers", async () => {
    dbSelect.mockResolvedValueOnce([{
      metadata: JSON.stringify({
        source: "漫画页",
        projectId: "project-1",
        novelWorkId: "work-1",
        visualRunId: "run-1",
        manifestFingerprint: "fingerprint-1",
        params: { pageNo: 3 },
      }),
    }]);
    dbExecute.mockResolvedValueOnce({ rowsAffected: 1 });

    await updateAssetMetadata(libAsset({ prompt: "新提示词" }), "手动修改", { prompt: "新提示词" });

    const written = JSON.parse(vi.mocked(dbExecute).mock.calls[0][1][0] as string);
    expect(written.novelWorkId).toBe("work-1");
    expect(written.visualRunId).toBe("run-1");
    expect(written.manifestFingerprint).toBe("fingerprint-1");
    expect(written.source).toBe("手动修改");
    expect(written.params).toEqual({ prompt: "新提示词" });
  });

  it("fails when the asset row does not exist", async () => {
    dbSelect.mockResolvedValueOnce([]);
    await expect(updateAssetMetadata(libAsset({}), "手动修改", {})).rejects.toThrow("资源不存在");
    expect(dbExecute).not.toHaveBeenCalled();
  });

  it("still writes when stored metadata is not valid JSON", async () => {
    dbSelect.mockResolvedValueOnce([{ metadata: "{not json" }]);
    dbExecute.mockResolvedValueOnce({ rowsAffected: 1 });
    await updateAssetMetadata(libAsset({ prompt: "x" }), "手动修改", { prompt: "x" });
    const written = JSON.parse(vi.mocked(dbExecute).mock.calls[0][1][0] as string);
    expect(written.source).toBe("手动修改");
    expect(written.params).toEqual({ prompt: "x" });
  });
});
