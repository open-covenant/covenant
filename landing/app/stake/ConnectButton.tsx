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
        className="font-mono text-[12px] text-fg-soft transition-colors hover:text-fg"
      >
        {shortAddr(address)}
      </button>
    );
  }

  return (
    <button
      type="button"
      onClick={() => open()}
      className="btn btn-primary"
    >
      connect wallet
    </button>
  );
}
