"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { ConnectButton } from "../stake/ConnectButton";
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
import { getClusterConfig, getReadConnection } from "../../lib/stake/env";
import {
  formatCvnt,
  formatSol,
  formatWithGrouping,
} from "../../lib/stake/format";
import { computeApy, formatApy } from "../../lib/stake/apy";
import { fetchCvntSolPrice, type CvntPrice } from "../../lib/stake/price";

interface TreasuryState {
  config: ConfigState;
  rewardVaultLamports: bigint;
  lockedVaultCvnt: bigint;
  buylockVaultCvnt: bigint;
  price: CvntPrice | null;
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
      const [rewardVaultLamports, lockedVaultCvnt, buylockVaultCvnt, price] =
        await Promise.all([
          fetchRewardVaultLamports(connection),
          fetchTokenAccountAmount(connection, lockedVault),
          fetchTokenAccountAmount(connection, buylockVault),
          fetchCvntSolPrice(),
        ]);
      if (!cancelled) {
        setState({
          config,
          rewardVaultLamports,
          lockedVaultCvnt: lockedVaultCvnt ?? 0n,
          buylockVaultCvnt: buylockVaultCvnt ?? 0n,
          price,
        });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [connection, cluster.cvntMint, cluster.tokenProgramId]);

  return (
    <div className="mx-auto w-full">
      <PageHeader />

      {!state && (
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {[0, 1, 2, 3].map((i) => (
            <div
              key={i}
              className="h-32 animate-pulse rounded-md border border-neutral-900 bg-neutral-950/40"
            />
          ))}
        </div>
      )}

      {state && <Dashboard state={state} />}
    </div>
  );
}

function Dashboard({ state }: { state: TreasuryState }) {
  const { config, rewardVaultLamports, lockedVaultCvnt, buylockVaultCvnt, price } = state;
  const lockUtilization =
    config.maxActiveLocks > 0
      ? Math.round((config.activeLockCount / config.maxActiveLocks) * 100)
      : 0;
  const apy = price
    ? computeApy({
        cumulativeSolDistributedLamports: config.cumulativeSolDistributed,
        initializedTsSeconds: config.initializedTs,
        nowSeconds: Math.floor(Date.now() / 1000),
        lockedCvntRawAmount: lockedVaultCvnt,
        solPerCvnt: price.solPerCvnt,
      })
    : null;

  return (
    <>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <KpiCard
          label="Annualized return (trailing)"
          value={apy ? formatApy(apy.pct) : "—"}
          unit={apy ? `${config.activeLockCount} / ${config.maxActiveLocks} positions` : "awaiting data"}
        />
        <KpiCard
          label="Lifetime SOL distributed"
          value={formatSol(config.cumulativeSolDistributed, { maxFrac: 4 })}
          unit="SOL"
        />
        <KpiCard
          label="Locked principal"
          value={formatCvnt(lockedVaultCvnt, { maxFrac: 0 })}
          unit="CVNT"
        />
        <KpiCard
          label="Protocol-held CVNT"
          value={formatCvnt(buylockVaultCvnt, { maxFrac: 0 })}
          unit="CVNT"
        />
      </div>

      <div className="mt-6 grid grid-cols-1 gap-6 lg:grid-cols-[1.1fr_0.9fr]">
        <Panel>
          <PanelEyebrow>Distribution mechanics</PanelEyebrow>
          <h2 className="mt-4 text-xl font-extralight tracking-tight text-neutral-50 sm:text-2xl">
            Protocol revenue, allocated by position weight
          </h2>
          <p className="mt-4 text-[13px] leading-relaxed text-neutral-400">
            Each protocol revenue tick is split four ways at the keeper layer.
            The staker share is then folded into a per-weight accumulator,
            ensuring every open position earns a strictly pro-rata share of
            every distribution since it was opened.
          </p>

          <div className="mt-8 grid grid-cols-1 gap-3 sm:grid-cols-2">
            <SplitCard label="Stakers" value="25%" detail="Distributed as SOL, claimable continuously." />
            <SplitCard label="Protocol CVNT" value="25%" detail="Swapped to CVNT and held by the protocol." />
            <SplitCard label="Treasury" value="30%" detail="Funds protocol operations and reserves." />
            <SplitCard label="Operator subsidy" value="20%" detail="Covers infrastructure and operational costs." />
          </div>

          <div className="mt-8 border-t border-neutral-900 pt-6">
            <PanelEyebrow>Accumulator</PanelEyebrow>
            <div className="mt-4 grid grid-cols-1 gap-3 sm:grid-cols-2">
              <DetailRow
                label="Total weight"
                value={`${formatCvnt(config.totalWeight, { maxFrac: 0 })} CVNT-weighted`}
              />
              <DetailRow
                label="Pending pre-accrual"
                value={`${formatSol(config.pendingSolLamports, { maxFrac: 4 })} SOL`}
              />
              <DetailRow
                label="Reward vault balance"
                value={`${formatSol(rewardVaultLamports, { maxFrac: 4 })} SOL`}
              />
              <DetailRow
                label="Minimum position"
                value={`${formatCvnt(config.minLockAmount, { maxFrac: 0 })} CVNT`}
              />
            </div>
          </div>
        </Panel>

        <div className="flex flex-col gap-6">
          <Panel>
            <PanelEyebrow>Status</PanelEyebrow>
            <div className="mt-6 flex items-center justify-between">
              <span className="text-[12px] uppercase tracking-[0.22em] text-neutral-500">
                Protocol state
              </span>
              <StatusPill ok={!config.paused} label={config.paused ? "Paused" : "Active"} />
            </div>
            <p className="mt-4 text-[12px] leading-relaxed text-neutral-500">
              When paused, new positions and reward claims are blocked. Principal
              withdrawal after lock expiry remains available regardless of pause
              state.
            </p>
          </Panel>

          <Panel>
            <PanelEyebrow>Lock tiers</PanelEyebrow>
            <div className="mt-6 grid grid-cols-1 gap-2">
              <TierLine days={7} multiplier="0.5×" />
              <TierLine days={30} multiplier="1.0×" />
              <TierLine days={90} multiplier="1.5×" />
              <TierLine days={180} multiplier="2.0×" />
            </div>
            <p className="mt-6 text-[11px] leading-relaxed text-neutral-500">
              A position&apos;s weight equals its principal multiplied by the
              tier coefficient. Distribution shares are computed from weight,
              not principal.
            </p>
            <div className="mt-6 border-t border-neutral-900 pt-4">
              <Link
                href="/stake"
                className="block rounded-sm bg-neutral-50 px-6 py-2.5 text-center text-[11px] uppercase tracking-[0.28em] text-neutral-950 transition-colors hover:bg-white"
              >
                Open a position
              </Link>
            </div>
          </Panel>
        </div>
      </div>

      <div className="mt-6">
        <Panel>
          <PanelEyebrow>Disclosures</PanelEyebrow>
          <ul className="mt-6 grid grid-cols-1 gap-4 text-[12px] leading-relaxed text-neutral-500 sm:grid-cols-2">
            <li>All values on this page are read directly from the program&apos;s on-chain state at the most recent confirmed slot.</li>
            <li>Distribution amounts reflect actual protocol revenue and are not guaranteed. Past distributions do not predict future amounts.</li>
            <li>Locked principal is non-transferable. Positions cannot be withdrawn before their lock period elapses.</li>
            <li>Protocol-held CVNT is acquired with protocol revenue. The current program version has no path to remove it.</li>
          </ul>
        </Panel>
      </div>
    </>
  );
}

function PageHeader() {
  return (
    <div className="mb-10 flex flex-col items-start gap-4 border-b border-neutral-900 pb-6 sm:flex-row sm:items-end sm:justify-between sm:gap-6">
      <div>
        <div className="text-[10px] uppercase tracking-[0.3em] text-neutral-500">Covenant Stake</div>
        <h1 className="mt-3 text-3xl font-extralight tracking-tight text-neutral-50 sm:text-4xl">
          Treasury
        </h1>
        <p className="mt-3 max-w-xl text-sm leading-relaxed text-neutral-400">
          A public, real-time view of the staking program&apos;s on-chain state. All
          numbers sourced from chain at the latest confirmed slot.
        </p>
      </div>
      <div className="block">
        <ConnectButton />
      </div>
    </div>
  );
}

function Panel({ children }: { children: React.ReactNode }) {
  return (
    <section className="rounded-md border border-neutral-900 bg-neutral-950/50 p-6 backdrop-blur-sm sm:p-8">
      {children}
    </section>
  );
}

function PanelEyebrow({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-[10px] uppercase tracking-[0.28em] text-neutral-500">{children}</div>
  );
}

function KpiCard({ label, value, unit }: { label: string; value: string; unit: string }) {
  return (
    <div className="rounded-md border border-neutral-900 bg-neutral-950/50 p-6">
      <div className="text-[10px] uppercase tracking-[0.22em] text-neutral-500">{label}</div>
      <div className="mt-4 flex items-baseline gap-2">
        <span className="font-mono text-2xl font-light tracking-tight text-neutral-50 sm:text-3xl">{value}</span>
        <span className="text-[10px] uppercase tracking-[0.22em] text-neutral-500">{unit}</span>
      </div>
    </div>
  );
}

function SplitCard({ label, value, detail }: { label: string; value: string; detail: string }) {
  return (
    <div className="rounded-sm border border-neutral-900 p-4">
      <div className="flex items-baseline justify-between">
        <span className="text-[11px] uppercase tracking-[0.22em] text-neutral-400">{label}</span>
        <span className="font-mono text-lg text-neutral-50">{value}</span>
      </div>
      <p className="mt-2 text-[11px] leading-relaxed text-neutral-500">{detail}</p>
    </div>
  );
}

function DetailRow({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-[10px] uppercase tracking-[0.2em] text-neutral-600">{label}</div>
      <div className="mt-1 font-mono text-sm text-neutral-100">{value}</div>
    </div>
  );
}

function StatusPill({ ok, label }: { ok: boolean; label: string }) {
  return (
    <span
      className={`inline-flex items-center gap-2 rounded-full border px-3 py-1 text-[10px] uppercase tracking-[0.22em] ${
        ok
          ? "border-emerald-500/40 bg-emerald-500/10 text-emerald-300"
          : "border-amber-500/40 bg-amber-500/10 text-amber-300"
      }`}
    >
      <span className={`h-1.5 w-1.5 rounded-full ${ok ? "bg-emerald-400" : "bg-amber-400"}`} />
      {label}
    </span>
  );
}

function TierLine({ days, multiplier }: { days: number; multiplier: string }) {
  return (
    <div className="flex items-baseline justify-between border-b border-neutral-900 pb-2 last:border-0 last:pb-0">
      <span className="text-[12px] text-neutral-300">{days} days</span>
      <span className="font-mono text-[13px] text-neutral-50">{multiplier}</span>
    </div>
  );
}
