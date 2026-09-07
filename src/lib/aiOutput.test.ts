import { describe, expect, it } from "vitest";
import { parseMarkedSections, stripThinking } from "./aiOutput";

describe("AI text output normalization", () => {
  it("removes complete and unfinished thinking blocks", () => {
    expect(stripThinking("<think>hidden</think>结果")).toBe("结果");
    expect(stripThinking("结果<think>unfinished")).toBe("结果");
  });

  it("keeps content that appears on the same line as a marker", () => {
    expect(parseMarkedSections("前言\n【恒定锚】角色黑发【版本锚】红色外套")).toEqual([
      { marker: "", body: "前言" },
      { marker: "恒定锚", body: "角色黑发" },
      { marker: "版本锚", body: "红色外套" },
    ]);
  });
});
