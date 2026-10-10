// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";
import { useEscapeKey } from "./useEscapeKey";

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
