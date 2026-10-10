// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useRef } from "react";
import { useEscapeKey, useModalLayer } from "./useEscapeKey";

function Layer({ name, onEscape, active = true }: { name: string; onEscape: (name: string) => void; active?: boolean }) {
  useEscapeKey(() => onEscape(name), active);
  return <input aria-label={name} />;
}

const press = (init: KeyboardEventInit = {}) => fireEvent.keyDown(window, { key: "Escape", ...init });

describe("useEscapeKey", () => {
  afterEach(() => cleanup());

  it("closes only the newest open layer", () => {
    const onEscape = vi.fn();
    const outer = render(<Layer name="modal" onEscape={onEscape} />);
    const inner = render(<Layer name="picker" onEscape={onEscape} />);

    press();
    expect(onEscape).toHaveBeenCalledTimes(1);
    expect(onEscape).toHaveBeenLastCalledWith("picker");

    inner.unmount();
    press();
    expect(onEscape).toHaveBeenLastCalledWith("modal");
    outer.unmount();
  });

  it("ignores other keys, inactive layers, handled events and IME composition", () => {
    const onEscape = vi.fn();
    const { rerender } = render(<Layer name="modal" onEscape={onEscape} active={false} />);
    press();
    expect(onEscape).not.toHaveBeenCalled();

    rerender(<Layer name="modal" onEscape={onEscape} />);
    fireEvent.keyDown(window, { key: "Enter" });
    press({ isComposing: true });
    const handled = new KeyboardEvent("keydown", { key: "Escape", cancelable: true });
    handled.preventDefault();
    window.dispatchEvent(handled);
    expect(onEscape).not.toHaveBeenCalled();

    press();
    expect(onEscape).toHaveBeenCalledWith("modal");
  });

  it("uses the latest callback without re-registering the layer", () => {
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = render(<Layer name="modal" onEscape={first} />);
    const top = render(<Layer name="picker" onEscape={vi.fn()} />);
    top.unmount();

    rerender(<Layer name="modal" onEscape={second} />);
    press();
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledWith("modal");
  });
});

function Modal({ name, onClose }: { name: string; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  useModalLayer(ref, onClose);
  return (
    <div ref={ref} role="dialog" aria-label={name}>
      <button type="button">{name} 第一</button>
      <button type="button" disabled>{name} 禁用</button>
      <button type="button">{name} 最后</button>
    </div>
  );
}

const tab = (shiftKey = false) => fireEvent.keyDown(document.activeElement ?? window, { key: "Tab", shiftKey });

describe("useModalLayer", () => {
  afterEach(() => cleanup());

  it("moves focus in, keeps Tab inside and returns focus to the opener", async () => {
    render(<button type="button">打开</button>);
    const opener = screen.getByRole("button", { name: "打开" });
    opener.focus();
    const modal = render(<Modal name="对话框" onClose={() => {}} />);
    await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: "对话框 第一" })));

    tab(true);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "对话框 最后" }));
    tab();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "对话框 第一" }));

    modal.unmount();
    expect(document.activeElement).toBe(opener);
  });

  it("lets only the newest dialog hold Tab and Escape", async () => {
    const closed: string[] = [];
    render(<Modal name="外层" onClose={() => closed.push("外层")} />);
    const inner = render(<Modal name="内层" onClose={() => closed.push("内层")} />);
    await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: "内层 第一" })));

    screen.getByRole("button", { name: "外层 最后" }).focus();
    tab();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "内层 第一" }));
    fireEvent.keyDown(window, { key: "Escape" });
    expect(closed).toEqual(["内层"]);
    inner.unmount();
  });
});
