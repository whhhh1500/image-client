// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import ModelCombobox from "./ModelCombobox";

afterEach(cleanup);

const optionNames = () =>
  screen.queryAllByRole("option").map((option) => option.textContent);

describe("ModelCombobox", () => {
  it("keeps a hand-typed model name that is not in the catalog", () => {
    const onChange = vi.fn();
    render(
      <ModelCombobox
        label="图像模型"
        value="gpt-image-2"
        options={["gpt-image-2"]}
        onChange={onChange}
      />,
    );

    fireEvent.change(screen.getByRole("combobox", { name: "图像模型" }), {
      target: { value: "my-private-model" },
    });

    expect(onChange).toHaveBeenCalledWith("my-private-model");
  });

  it("offers the catalog, filters while typing and selects an option", () => {
    const onChange = vi.fn();
    render(
      <ModelCombobox
        label="视频模型"
        value=""
        options={["kling-video-v3", "minimax-h3"]}
        onChange={onChange}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "展开视频模型列表" }));
    expect(optionNames()).toEqual(["kling-video-v3", "minimax-h3"]);

    fireEvent.change(screen.getByRole("combobox", { name: "视频模型" }), {
      target: { value: "mini" },
    });
    expect(optionNames()).toEqual(["minimax-h3"]);

    fireEvent.click(screen.getByRole("option", { name: "minimax-h3" }));
    expect(onChange).toHaveBeenCalledWith("minimax-h3");
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("selects the highlighted candidate with the keyboard", () => {
    const onChange = vi.fn();
    render(
      <ModelCombobox
        label="LLM 模型"
        value=""
        options={["gemini-3.7-flash", "gemini-3.6-flash"]}
        onChange={onChange}
      />,
    );

    const input = screen.getByRole("combobox", { name: "LLM 模型" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onChange).toHaveBeenCalledWith("gemini-3.6-flash");
  });

  it("asks the backend for the catalog from the trailing button", () => {
    const onFetch = vi.fn();
    const props = {
      label: "LLM 模型",
      value: "gemini-3.7-flash",
      options: [] as string[],
      onChange: vi.fn(),
      onFetch,
    };
    const { rerender } = render(<ModelCombobox {...props} fetching={false} />);

    fireEvent.click(screen.getByRole("button", { name: "获取模型（LLM 模型）" }));
    expect(onFetch).toHaveBeenCalledOnce();

    rerender(<ModelCombobox {...props} fetching />);
    const button = screen.getByRole("button", { name: "获取模型（LLM 模型）" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(button.textContent).toContain("获取中");

    rerender(
      <ModelCombobox
        {...props}
        fetching={false}
        status={{ text: "获取模型失败：模型目录返回 401", error: true }}
      />,
    );
    expect(screen.getByText("获取模型失败：模型目录返回 401")).toBeTruthy();
  });
});
