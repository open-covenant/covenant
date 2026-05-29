"use client";

import { useAppKit, useAppKitAccount } from "@reown/appkit/react";
import { shortAddr } from "../../lib/stake/format";

export function ConnectButton() {
  const { open } = useAppKit();
  const { address, isConnected } = useAppKitAccount();

  if (isConnected && address) {
    return (
      <button
        type="button"
        onClick={() => open({ view: "Account" })}
        className="font-mono text-[12px] text-neutral-300 transition-colors hover:text-neutral-50"
      >
        {shortAddr(address)}
      </button>
    );
  }

  return (
    <button
      type="button"
      onClick={() => open()}
      className="rounded-sm border border-neutral-200 bg-neutral-100 px-5 py-2 text-[11px] uppercase tracking-[0.28em] text-neutral-950 transition-colors hover:bg-white"
    >
      connect wallet
    </button>
  );
}
