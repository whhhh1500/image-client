import { describe, expect, it } from "vitest";
import { errorText, readableErrorText } from "./errors";

describe("errorText", () => {
  it("shows an Error's message without the Error: prefix", () => {
    expect(errorText(new Error("图像服务没有返回图像产物，请重试。"))).toBe("图像服务没有返回图像产物，请重试。");
    expect(errorText(new TypeError("bad"))).toBe("bad");
  });

  it("keeps backend strings and other values as they are", () => {
    expect(errorText("图像接口返回 400 Bad Request: 模型请求失败")).toBe("图像接口返回 400 Bad Request: 模型请求失败");
    expect(errorText(42)).toBe("42");
    expect(errorText(undefined)).toBe("undefined");
  });
});

describe("readableErrorText", () => {
  it("reduces a recorded gateway JSON body to its reason", () => {
    expect(readableErrorText('图像接口返回 500 Internal Server Error: {"error":{"message":"upstream error: do request failed","type":"new_api_error"}}'))
      .toBe("图像接口返回 500 Internal Server Error: upstream error: do request failed");
    expect(readableErrorText('{"error":"quota exhausted"}')).toBe("quota exhausted");
    expect(readableErrorText('文本接口返回 429: {"message":"rate limited"}')).toBe("文本接口返回 429: rate limited");
  });

  it("leaves other text unchanged", () => {
    expect(readableErrorText("网关超时")).toBe("网关超时");
    expect(readableErrorText("返回 {不是 JSON")).toBe("返回 {不是 JSON");
    expect(readableErrorText('返回 400: {"error":{"code":"x"}}')).toBe('返回 400: {"error":{"code":"x"}}');
  });
});
