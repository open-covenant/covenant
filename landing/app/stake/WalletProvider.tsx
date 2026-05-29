"use client";

import { ReactNode, useMemo } from "react";
import { createAppKit } from "@reown/appkit/react";
import {
  solana,
  solanaDevnet,
} from "@reown/appkit/networks";
import { SolanaAdapter } from "@reown/appkit-adapter-solana/react";
import {
  PhantomWalletAdapter,
  SolflareWalletAdapter,
} from "@solana/wallet-adapter-wallets";
import { getClusterConfig } from "../../lib/stake/env";

const projectId = "25ff9d8aeef329151de0bf83bd83a38b";

const metadata = {
  name: "Covenant Stake",
  description: "Lock $CVNT and share in Covenant protocol revenue, paid in SOL.",
  url: "https://opencovenant.org",
  icons: ["https://opencovenant.org/icon.png"],
};

const solanaAdapter = new SolanaAdapter({
  wallets: [new PhantomWalletAdapter(), new SolflareWalletAdapter()],
});

let appKitInitialized = false;
function ensureAppKit() {
  if (appKitInitialized) return;
  appKitInitialized = true;
  const { cluster } = getClusterConfig();
  createAppKit({
    adapters: [solanaAdapter],
    projectId,
    networks: cluster === "mainnet-beta" ? [solana] : [solanaDevnet],
    defaultNetwork: cluster === "mainnet-beta" ? solana : solanaDevnet,
    metadata,
    features: { analytics: true },
    themeMode: "dark",
    themeVariables: {
      "--w3m-color-mix": "#030303",
      "--w3m-color-mix-strength": 40,
      "--w3m-accent": "#f5f5f5",
      "--w3m-font-family":
        'ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif',
      "--w3m-border-radius-master": "2px",
    },
  });
}

export function WalletProvider({ children }: { children: ReactNode }) {
  useMemo(() => ensureAppKit(), []);
  return <>{children}</>;
}
