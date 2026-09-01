import notifier from "node-notifier";
import { t } from "./i18n.ts";

// Native Windows toast notifications with burst coalescing: several
// messages arriving together become a single summary toast. Clicking a
// toast reports the conversation it came from.
export class Notify {
  private queue: { title: string; body: string; icon: string | null; jid: string }[] = [];
  private timer: NodeJS.Timeout | null = null;

  // Set by the bridge to focus the conversation the toast belongs to.
  onActivate: ((jid: string) => void) | null = null;

  push(title: string, body: string, icon: string | null = null, jid = "") {
    this.queue.push({ title, body, icon, jid });
    if (!this.timer) {
      this.timer = setTimeout(() => this.flush(), 1200);
    }
  }

  private show(title: string, message: string, icon: string | null, jid: string) {
    try {
      notifier.notify(
        {
          title,
          message: message || " ",
          icon: icon ?? undefined,
          appID: "Zapive",
          wait: true, // required for the activation callback
        } as never,
        (err, response, metadata) => {
          // SnoreToast reports the click through whichever of the three
          // arguments it feels like: an "activated" response, an error
          // carrying the same word, or metadata.activationType.
          const hint = [
            err instanceof Error ? err.message : String(err ?? ""),
            String(response ?? ""),
            JSON.stringify(metadata ?? {}),
          ]
            .join(" ")
            .toLowerCase();
          // Only the ways a toast can end WITHOUT being clicked are
          // reported by name; a click comes back as an empty result, so
          // anything that is not one of those counts as an activation.
          const inert = /timedout|dismissed|hidden|failed|error/.test(hint);
          console.log(`[notify] ${inert ? "inert" : "activate"} ${hint.trim()}`);
          if (jid && !inert) this.onActivate?.(jid);
        },
      );
    } catch (err) {
      console.error("notification failed:", err);
    }
  }

  private flush() {
    this.timer = null;
    const items = this.queue.splice(0);
    if (items.length === 0) return;
    if (items.length === 1) {
      const item = items[0]!;
      this.show(item.title, item.body, item.icon, item.jid);
      return;
    }
    const names = [...new Set(items.map((i) => i.title))];
    const summary =
      names.length === 1
        ? t("notify.fromOne", items.length, names[0]!)
        : t("notify.newMessages", items.length, names.slice(0, 3).join(", "));
    const latest = items[items.length - 1]!;
    this.show(t("notify.appName"), summary, latest.icon, latest.jid);
  }
}
