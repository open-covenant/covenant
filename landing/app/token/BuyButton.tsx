"use client";

import type { MouseEvent } from "react";

// Opens the Qwerti buy widget — the same thing the floating trigger (bottom-left)
// opens. We call the widget's public API; if you click before the widget script
// has finished loading we wait briefly rather than bouncing to pump.fun. We open
// on the next tick so this click doesn't trip the widget's own
// outside-click-to-close handler. pump.fun stays as a last-resort fallback.

const PUMP_FUN_URL =
  "https://pump.fun/coin/2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump";

type QwertiApi = { openWidget?: () => void };

function tryOpenWidget(): boolean {
  const q = (window as unknown as { Qwerti?: QwertiApi }).Qwerti;
  if (typeof q?.openWidget === "function") {
    q.openWidget();
    return true;
  }
  // Fallback: click the widget's own floating trigger if the API isn't exposed.
  const trigger = document.querySelector<HTMLButtonElement>(".qwerti-trigger");
  if (trigger) {
    trigger.click();
    return true;
  }
  return false;
}

export function BuyButton({ className }: { className?: string }) {
  function buy(e: MouseEvent<HTMLButtonElement>) {
    e.stopPropagation();
    // Defer to the next tick so this click finishes propagating before the
    // widget opens; otherwise the widget's outside-click handler closes it.
    window.setTimeout(() => {
      if (tryOpenWidget()) return;
      // Widget script not ready yet: poll up to ~3s, then fall back.
      let tries = 0;
      const id = window.setInterval(() => {
        if (tryOpenWidget() || ++tries >= 20) {
          window.clearInterval(id);
          if (tries >= 20) {
            window.open(PUMP_FUN_URL, "_blank", "noopener,noreferrer");
          }
        }
      }, 150);
    }, 0);
  }

  return (
    <button type="button" onClick={buy} className={className}>
      Buy $CVNT
    </button>
  );
}
