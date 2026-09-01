import notifier from "node-notifier";
import { t } from "./i18n.ts";

// Native Windows toast notifications with burst coalescing: several
// messages arriving together become a single summary toast.
export class Notify {
  private queue: { title: string; body: string; icon: string | null }[] = [];
  private timer: NodeJS.Timeout | null = null;

  push(title: string, body: string, icon: string | null = null) {
    this.queue.push({ title, body, icon });
    if (!this.timer) {
      this.timer = setTimeout(() => this.flush(), 1200);
    }
  }

  private flush() {
    this.timer = null;
    const items = this.queue.splice(0);
    if (items.length === 0) return;
    try {
      if (items.length === 1) {
        const item = items[0]!;
        notifier.notify({
          title: item.title,
          message: item.body || " ",
          icon: item.icon ?? undefined,
          appID: "Zapive",
        } as never);
      } else {
        const titles = [...new Set(items.map((i) => i.title))].slice(0, 3).join(", ");
        notifier.notify({
          title: "Zapive",
          message: t("notify.newMessages", items.length, titles),
          icon: items.find((i) => i.icon)?.icon ?? undefined,
          appID: "Zapive",
        } as never);
      }
    } catch (err) {
      console.error("notification failed:", err);
    }
  }
}
