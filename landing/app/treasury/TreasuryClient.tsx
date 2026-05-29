"use client";

import { useEffect, useMemo, useState } from "react";
import { getReadConnection } from "../../lib/stake/env";
import {
  fetchConfig,
  fetchRewardVaultLamports,
  fetchTokenAccountAmount,
  type ConfigState,
} from "../../lib/stake/readers";
import {
  buylockVaultAuthorityPda,
  deriveAta,
  lockedVaultAuthorityPda,
} from "../../lib/stake/pdas";
import { getClusterConfig } from "../../lib/stake/env";
import { formatCvnt, formatSol } from "../../lib/stake/format";

interface TreasuryState {
  config: ConfigState;
  rewardVaultLamports: bigint;
  lockedVaultCvnt: bigint;
  buylockVaultCvnt: bigint;
}

export function TreasuryClient() {
  const connection = useMemo(() => getReadConnection(), []);
  const cluster = getClusterConfig();
  const [state, setState] = useState<TreasuryState | null>(null);

  useEffect(() => {
    if (!connection) return;
    let cancelled = false;
    (async () => {
      const config = await fetchConfig(connection);
      if (!config) return;
      const lockedVault = deriveAta(
        lockedVaultAuthorityPda(),
        cluster.cvntMint,
        cluster.tokenProgramId,
      );
      const buylockVault = deriveAta(
        buylockVaultAuthorityPda(),
        cluster.cvntMint,
        cluster.tokenProgramId,
      );
      const [rewardVaultLamports, lockedVaultCvnt, buylockVaultCvnt] =
        await Promise.all([
          fetchRewardVaultLamports(connection),
          fetchTokenAccountAmount(connection, lockedVault),
          fetchTokenAccountAmount(connection, buylockVault),
        ]);
      if (!cancelled) {
        setState({
          config,
          rewardVaultLamports,
          lockedVaultCvnt: lockedVaultCvnt ?? 0n,
          buylockVaultCvnt: buylockVaultCvnt ?? 0n,
        });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [connection, cluster.cvntMint, cluster.tokenProgramId]);

  if (!state) {
    return (
      <div className="mx-auto max-w-3xl">
        <h1 className="text-3xl font-extralight tracking-tight text-neutral-50 sm:text-4xl">
          Treasury
        </h1>
        <p className="mt-6 text-sm text-neutral-500">loading…</p>
      </div>
    );
  }

  const { config, rewardVaultLamports, lockedVaultCvnt, buylockVaultCvnt } = state;

  return (
    <div className="mx-auto max-w-3xl">
      <h1 className="text-3xl font-extralight tracking-tight text-neutral-50 sm:text-4xl">
        Treasury
      </h1>
      <p className="mt-3 text-sm leading-relaxed text-neutral-400">
        Public read-only view of the staking program state. All values fetched
        directly from chain.
      </p>

      <div className="mt-10 grid grid-cols-1 gap-3 sm:grid-cols-2">
        <Stat
          label="cumulative SOL distributed"
          value={`${formatSol(config.cumulativeSolDistributed, { maxFrac: 4 })} SOL`}
        />
        <Stat
          label="reward vault balance"
          value={`${formatSol(rewardVaultLamports, { maxFrac: 4 })} SOL`}
        />
        <Stat
          label="active positions"
          value={`${config.activeLockCount} / ${config.maxActiveLocks}`}
        />
        <Stat
          label="locked principal"
          value={`${formatCvnt(lockedVaultCvnt, { maxFrac: 0 })} CVNT`}
        />
        <Stat
          label="bought + locked (buyback vault)"
          value={`${formatCvnt(buylockVaultCvnt, { maxFrac: 0 })} CVNT`}
          help="protocol revenue auto-buys $CVNT and locks it here — no withdraw path in v1"
        />
        <Stat
          label="pending pre-accrual"
          value={`${formatSol(config.pendingSolLamports, { maxFrac: 4 })} SOL`}
        />
        <Stat
          label="total weight"
          value={config.totalWeight.toString()}
          help="sum of amount × tier_multiplier across active positions"
        />
        <Stat
          label="protocol status"
          value={config.paused ? "paused" : "active"}
          tone={config.paused ? "warn" : "ok"}
        />
      </div>

      <p className="mt-8 text-[11px] leading-relaxed text-neutral-500">
        This is real yield, not inflation. Revenue is sourced from pump.fun
        creator fees on the post-graduation $CVNT pool, swept by the keeper
        on a fixed cadence. 25% of each sweep flows to stakers, 25% to a
        buy-and-lock vault, 30% to treasury, 20% to operator subsidy. The
        program emits zero $CVNT.
      </p>
    </div>
  );
}

function Stat({
  label,
  value,
  help,
  tone,
}: {
  label: string;
  value: string;
  help?: string;
  tone?: "ok" | "warn";
}) {
  const accent =
    tone === "warn"
      ? "text-amber-300"
      : tone === "ok"
        ? "text-emerald-300"
        : "text-neutral-50";
  return (
    <div className="rounded-md border border-neutral-800 bg-neutral-950/60 p-5">
      <div className="text-[10px] uppercase tracking-[0.18em] text-neutral-500">
        {label}
      </div>
      <div className={`mt-2 font-mono text-xl ${accent}`}>{value}</div>
      {help && <div className="mt-2 text-[11px] text-neutral-500">{help}</div>}
    </div>
  );
}
