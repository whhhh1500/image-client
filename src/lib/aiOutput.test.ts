import { describe, expect, it } from "vitest";
import { parseMarkedSections, parseStoryboardShots, stripThinking } from "./aiOutput";

describe("AI output normalization", () => {
  it("removes complete and unfinished thinking blocks", () => {
    expect(stripThinking("<think>hidden</think>结果")).toBe("结果");
    expect(stripThinking("结果<think>unfinished")).toBe("结果");
  });

  it("parses fenced storyboard JSON with surrounding prose", () => {
    const shots = parseStoryboardShots(`说明\n\`\`\`json
      {"shots":[{"shotNo":3,"shotType":"近景","prompt":"角色回头"}]}
    \`\`\`\n结束`);
    expect(shots).toEqual([{ shotNo: 3, shotType: "近景", prompt: "角色回头" }]);
  });

  it("ignores invalid shot entries", () => {
    expect(parseStoryboardShots('{"shots":[null,{}, {"action":"奔跑"}]}')).toEqual([
      { shotNo: 3, action: "奔跑" },
    ]);
  });

  it("keeps content that appears on the same line as a marker", () => {
    expect(parseMarkedSections("前言\n【恒定锚】角色黑发【版本锚】红色外套")).toEqual([
      { marker: "", body: "前言" },
      { marker: "恒定锚", body: "角色黑发" },
      { marker: "版本锚", body: "红色外套" },
    ]);
  });
});
