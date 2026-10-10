import { confirmDialog, isDesktop } from "./desktop";
import { logEvent } from "./logger";

export async function confirmAction(message: string): Promise<boolean> {
  try {
    return isDesktop()
      ? await confirmDialog(message, { title: "确认操作", okLabel: "确定", cancelLabel: "取消" })
      : await window.confirm(message);
  } catch (error) {
    logEvent("error", "dialog.confirm_failed", { error: String(error) });
    return false;
  }
}
