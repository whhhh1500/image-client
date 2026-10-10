import { beforeEach, describe, expect, it, vi } from "vitest";

const { dbSelect, dbExecute } = vi.hoisted(() => ({ dbSelect: vi.fn(), dbExecute: vi.fn() }));

vi.mock("./db", () => ({ dbSelect, dbExecute }));
vi.mock("./logger", () => ({ logEvent: vi.fn() }));
vi.mock("./ipc", () => ({ persistAssetsBatch: vi.fn() }));

import { loadHistory, markTaskRetried } from "./dbWrite";

const storedInput = {
  model: "gpt-image-2",
  label: "文生图",
  projectId: "",
  kind: "image",
  params: { prompt: "雨夜", model: "gpt-image-2" },
};

describe("markTaskRetried", () => {
  beforeEach(() => {
    dbSelect.mockReset();
    dbExecute.mockReset();
  });

  it("adds retriedBy to the stored params and keeps the rest of the row", async () => {
    dbSelect.mockResolvedValueOnce([{ input_json: JSON.stringify(storedInput) }]);
    dbExecute.mockResolvedValueOnce({ rowsAffected: 1 });

    await markTaskRetried("task-old", "task-new");

    const [sql, values] = vi.mocked(dbExecute).mock.calls[0];
    expect(sql).toBe("UPDATE tasks SET input_json = ? WHERE id = ?");
    expect(values[1]).toBe("task-old");
    expect(JSON.parse(values[0] as string)).toEqual({ ...storedInput, params: { ...storedInput.params, retriedBy: "task-new" } });
  });

  it("survives a history reload as part of the task params", async () => {
    dbSelect.mockResolvedValueOnce([{ input_json: JSON.stringify(storedInput) }]);
    dbExecute.mockResolvedValueOnce({ rowsAffected: 1 });
    await markTaskRetried("task-old", "task-new");
    const written = vi.mocked(dbExecute).mock.calls[0][1][0] as string;

    dbSelect
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([{ id: "task-old", node_id: "gen_txt2img", status: "error", input_json: written, error: "超时", created_at: 1, finished_at: 2 }]);
    const { tasks } = await loadHistory();

    expect(tasks[0]).toMatchObject({ id: "task-old", status: "error", label: "文生图", params: { prompt: "雨夜", retriedBy: "task-new" } });
  });

  it("refuses to rewrite a missing or unreadable row", async () => {
    dbSelect.mockResolvedValueOnce([]);
    await expect(markTaskRetried("missing", "task-new")).rejects.toThrow("任务记录不存在");

    dbSelect.mockResolvedValueOnce([{ input_json: "{not json" }]);
    await expect(markTaskRetried("broken", "task-new")).rejects.toThrow();
    expect(dbExecute).not.toHaveBeenCalled();
  });
});
