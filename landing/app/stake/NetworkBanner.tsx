"use client";

import { getClusterConfig } from "../../lib/stake/env";

export function NetworkBanner() {
  const { cluster } = getClusterConfig();
  if (cluster === "mainnet-beta") return null;
  return (
    <div className="pointer-events-none absolute inset-x-0 top-0 z-30 flex justify-center pt-[max(72px,env(safe-area-inset-top))] sm:pt-[max(86px,env(safe-area-inset-top))]">
      <div className="pointer-events-auto rounded-full border border-warn/30 bg-warn/10 px-3 py-1 text-[10px] uppercase tracking-[0.2em] text-warn backdrop-blur-sm">
        {cluster} · test tokens only
      </div>
    </div>
  );
}
