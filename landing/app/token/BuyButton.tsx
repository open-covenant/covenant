"use client";

// The Qwerti widget renders as a body-level modal + floating trigger; it never
// mounts inline. This button drives it programmatically via window.Qwerti, with
// a pump.fun fallback if the widget script hasn't loaded (or is blocked).

const PUMP_FUN_URL =
  "https://pump.fun/coin/2mNVZ6aEjrGwiUVCfz7XGWpiXuWzgBDoznwE579upump";

type QwertiApi = { openWidget?: () => void };

export function BuyButton({ className }: { className?: string }) {
  function buy() {
    const q = (window as unknown as { Qwerti?: QwertiApi }).Qwerti;
    if (q?.openWidget) {
      q.openWidget();
    } else {
      window.open(PUMP_FUN_URL, "_blank", "noopener,noreferrer");
    }
  }

  return (
    <button type="button" onClick={buy} className={className}>
      Buy $CVNT
    </button>
  );
}
