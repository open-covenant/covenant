"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { useAppKitAccount, useAppKitProvider } from "@reown/appkit/react";
import { type Provider } from "@reown/appkit-adapter-solana/react";
import { PublicKey, Transaction } from "@solana/web3.js";
import { ConnectButton } from "../stake/ConnectButton";
import { explorerAddressUrl, explorerTxUrl, getReadConnection } from "../../lib/stake/env";
import {
  buildClaimIx,
  buildClosePositionIx,
  buildCreateAtaIx,
} from "../../lib/stake/txBuilder";
import { deriveAta } from "../../lib/stake/pdas";
import { getClusterConfig } from "../../lib/stake/env";
import {
  computePendingLamports,
  fetchConfig,
  fetchOwnerPositions,
  type ConfigState,
  type StakePositionState,
} from "../../lib/stake/readers";
import {
  formatCvnt,
  formatSol,
  lockEndDate,
  relativeFromNow,
  shortAddr,
  tierLabel,
  tierMultiplier,
} from "../../lib/stake/format";

type TxState =
  | { kind: "idle" }
  | { kind: "submitting"; positionPubkey: string; action: "claim" | "close" }
  | { kind: "ok"; sig: string }
  | { kind: "error"; message: string };

export function PositionsClient() {
  const connection = useMemo(() => getReadConnection(), []);
  const { address } = useAppKitAccount();
  const { walletProvider } = useAppKitProvider<Provider>("solana");
  const publicKey = useMemo(
    () => (address ? new PublicKey(address) : null),
    [address],
  );
  const [config, setConfig] = useState<ConfigState | null>(null);
  const [positions, setPositions] = useState<StakePositionState[] | null>(null);
  const [tx, setTx] = useState<TxState>({ kind: "idle" });
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    if (!publicKey || !connection) return;
    setLoading(true);
    const [c, p] = await Promise.all([
      fetchConfig(connection),
      fetchOwnerPositions(connection, publicKey),
    ]);
    setConfig(c);
    setPositions(p.sort((a, b) => Number(a.lockEnd - b.lockEnd)));
    setLoading(false);
  }, [connection, publicKey]);

  useEffect(() => {
    void refresh();
  }, [refresh, tx]);

  const handleClaim = async (pos: StakePositionState) => {
    if (!publicKey || !walletProvider || !connection) return;
    setTx({ kind: "submitting", positionPubkey: pos.pubkey.toBase58(), action: "claim" });
    try {
      const { blockhash, lastValidBlockHeight } =
        await connection.getLatestBlockhash("confirmed");
      const transaction = new Transaction({
        feePayer: publicKey,
        recentBlockhash: blockhash,
      });
      transaction.add(buildClaimIx({ owner: publicKey, nonce: pos.nonce }));
      const sig = await walletProvider.sendTransaction(transaction, connection);
      await connection.confirmTransaction(
        { signature: sig, blockhash, lastValidBlockHeight },
        "confirmed",
      );
      setTx({ kind: "ok", sig });
    } catch (e) {
      setTx({ kind: "error", message: e instanceof Error ? e.message : String(e) });
    }
  };

  const handleClose = async (pos: StakePositionState) => {
    if (!publicKey || !walletProvider || !connection) return;
    setTx({ kind: "submitting", positionPubkey: pos.pubkey.toBase58(), action: "close" });
    try {
      const cluster = getClusterConfig();
      const ownerAta = deriveAta(publicKey, cluster.cvntMint, cluster.tokenProgramId);
      const ataInfo = await connection.getAccountInfo(ownerAta, "confirmed");
      const ixs = [];
      if (!ataInfo) {
        ixs.push(buildCreateAtaIx({ payer: publicKey, owner: publicKey }));
      }
      ixs.push(buildClosePositionIx({ owner: publicKey, nonce: pos.nonce }));

      const { blockhash, lastValidBlockHeight } =
        await connection.getLatestBlockhash("confirmed");
      const transaction = new Transaction({
        feePayer: publicKey,
        recentBlockhash: blockhash,
      });
      transaction.add(...ixs);
      const sig = await walletProvider.sendTransaction(transaction, connection);
      await connection.confirmTransaction(
        { signature: sig, blockhash, lastValidBlockHeight },
        "confirmed",
      );
      setTx({ kind: "ok", sig });
    } catch (e) {
      setTx({ kind: "error", message: e instanceof Error ? e.message : String(e) });
    }
  };

  const totalLocked = useMemo(
    () => (positions ?? []).reduce((s, p) => s + p.amount, 0n),
    [positions],
  );
  const totalWeight = useMemo(
    () => (positions ?? []).reduce((s, p) => s + p.weight, 0n),
    [positions],
  );
  const totalClaimable = useMemo(() => {
    if (!config || !positions) return 0n;
    return positions.reduce(
      (s, p) => s + computePendingLamports(p, config.accSolPerWeight),
      0n,
    );
  }, [config, positions]);
  const now = BigInt(Math.floor(Date.now() / 1000));
  const unlockedCount = useMemo(
    () => (positions ?? []).filter((p) => p.lockEnd <= now).length,
    [positions, now],
  );

  return (
    <div className="mx-auto w-full">
      <PageHeader title="Positions" subtitle="Manage open positions, claim accrued rewards, and withdraw vested principal." />

      <div className="grid grid-cols-1 gap-6 lg:grid-cols-[0.9fr_1.1fr]">
        <div className="flex flex-col gap-6">
          <Panel>
            <PanelEyebrow>Account summary</PanelEyebrow>
            {!publicKey ? (
              <div className="mt-8 text-center">
                <ConnectButton />
                <p className="mt-4 text-[11px] text-neutral-500">Connect to view your positions.</p>
              </div>
            ) : (
              <div className="mt-6 grid grid-cols-1 gap-4">
                <StatRow
                  label="Total locked"
                  value={`${formatCvnt(totalLocked, { maxFrac: 2 })} CVNT`}
                />
                <StatRow
                  label="Total weight"
                  value={`${formatCvnt(totalWeight, { maxFrac: 0 })} CVNT-weighted`}
                />
                <StatRow
                  label="Claimable now"
                  value={`${formatSol(totalClaimable, { maxFrac: 6 })} SOL`}
                />
                <StatRow
                  label="Positions"
                  value={`${positions?.length ?? "—"} (${unlockedCount} unlocked)`}
                />
              </div>
            )}

            {publicKey && (
              <div className="mt-6 border-t border-neutral-900 pt-4 text-[10px] uppercase tracking-[0.22em] text-neutral-600">
                <Link href="/stake" className="transition-colors hover:text-neutral-300">
                  Open another position →
                </Link>
              </div>
            )}
          </Panel>

          <Panel>
            <PanelEyebrow>Position lifecycle</PanelEyebrow>
            <ol className="mt-6 space-y-5 text-[13px] leading-relaxed text-neutral-400">
              <Step
                index="01"
                title="Claim accrued rewards"
                body="Available at any moment without affecting your position. Claim transfers pending SOL to your wallet and resets the position's reward debt."
              />
              <Step
                index="02"
                title="Withdraw principal at lock end"
                body="Once the lock period elapses, close the position to return the locked CVNT to your wallet. Any pending SOL is paid out in the same transaction."
              />
              <Step
                index="03"
                title="Pause does not trap principal"
                body="If the protocol is paused, claims and new positions are blocked. Principal withdrawal after lock expiry remains available."
              />
            </ol>
          </Panel>
        </div>

        <Panel>
          <PanelEyebrow>Open positions</PanelEyebrow>

          {!publicKey && (
            <p className="mt-10 text-center text-[12px] text-neutral-500">
              Connect a wallet to view your positions.
            </p>
          )}

          {publicKey && positions === null && loading && (
            <PositionSkeleton />
          )}

          {publicKey && positions !== null && positions.length === 0 && !loading && (
            <div className="mt-12 text-center">
              <p className="text-[12px] text-neutral-500">No open positions in this wallet.</p>
              <Link
                href="/stake"
                className="mt-6 inline-block rounded-sm bg-neutral-50 px-6 py-2.5 text-[11px] uppercase tracking-[0.28em] text-neutral-950 transition-colors hover:bg-white"
              >
                Open position
              </Link>
            </div>
          )}

          {publicKey && positions !== null && positions.length > 0 && (
            <div className="mt-6 space-y-3">
              {positions.map((pos) => {
                const pending = config
                  ? computePendingLamports(pos, config.accSolPerWeight)
                  : 0n;
                const expired = pos.lockEnd <= now;
                const busy =
                  tx.kind === "submitting" &&
                  tx.positionPubkey === pos.pubkey.toBase58();
                return (
                  <PositionCard
                    key={pos.pubkey.toBase58()}
                    position={pos}
                    pending={pending}
                    expired={expired}
                    busy={busy}
                    busyAction={tx.kind === "submitting" ? tx.action : null}
                    onClaim={() => handleClaim(pos)}
                    onClose={() => handleClose(pos)}
                  />
                );
              })}
            </div>
          )}

          <div className="mt-6 min-h-[1.25rem] text-center text-[11px]">
            {tx.kind === "ok" && (
              <span className="text-emerald-300">
                Done.{" "}
                <a href={explorerTxUrl(tx.sig)} target="_blank" rel="noopener noreferrer" className="underline">
                  View transaction
                </a>
              </span>
            )}
            {tx.kind === "error" && (
              <span className="break-all text-rose-300">{tx.message}</span>
            )}
          </div>
        </Panel>
      </div>
    </div>
  );
}

function PageHeader({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div className="mb-10 flex flex-col items-start gap-4 border-b border-neutral-900 pb-6 sm:flex-row sm:items-end sm:justify-between sm:gap-6">
      <div>
        <div className="text-[10px] uppercase tracking-[0.3em] text-neutral-500">Covenant Stake</div>
        <h1 className="mt-3 text-3xl font-extralight tracking-tight text-neutral-50 sm:text-4xl">
          {title}
        </h1>
        <p className="mt-3 max-w-xl text-sm leading-relaxed text-neutral-400">{subtitle}</p>
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

function StatRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between border-b border-neutral-900 pb-3 last:border-0 last:pb-0">
      <div className="text-[11px] uppercase tracking-[0.2em] text-neutral-500">{label}</div>
      <div className="font-mono text-base text-neutral-100">{value}</div>
    </div>
  );
}

function Step({ index, title, body }: { index: string; title: string; body: string }) {
  return (
    <li className="grid grid-cols-[2rem_1fr] gap-4">
      <span className="pt-1 font-mono text-[11px] text-neutral-600">{index}</span>
      <div>
        <div className="text-[13px] font-medium text-neutral-100">{title}</div>
        <div className="mt-1 text-[12px] leading-relaxed text-neutral-500">{body}</div>
      </div>
    </li>
  );
}

function PositionCard({
  position,
  pending,
  expired,
  busy,
  busyAction,
  onClaim,
  onClose,
}: {
  position: StakePositionState;
  pending: bigint;
  expired: boolean;
  busy: boolean;
  busyAction: "claim" | "close" | null;
  onClaim: () => void;
  onClose: () => void;
}) {
  return (
    <div className="rounded-md border border-neutral-900 bg-neutral-950/60 p-5 transition-colors hover:border-neutral-800 sm:p-6">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <div className="font-mono text-2xl font-light tracking-tight text-neutral-50 sm:text-3xl">
            {formatCvnt(position.amount, { maxFrac: 2 })}
            <span className="ml-2 text-[10px] uppercase tracking-[0.22em] text-neutral-600">CVNT</span>
          </div>
          <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] uppercase tracking-[0.2em] text-neutral-500">
            <span>{tierLabel(position.multiplierBps)}</span>
            <span className="text-neutral-700">·</span>
            <span>Weight {formatCvnt(position.weight, { maxFrac: 0 })} CVNT-weighted</span>
          </div>
        </div>
        <div className="text-right">
          <div className="text-[10px] uppercase tracking-[0.22em] text-neutral-500">Claimable</div>
          <div className="mt-1 font-mono text-base text-emerald-300">
            {formatSol(pending, { maxFrac: 6 })} SOL
          </div>
        </div>
      </div>

      <div className="mt-4 grid grid-cols-2 gap-4 border-t border-neutral-900 pt-4 text-[11px] sm:grid-cols-4">
        <Metric label="Unlock date" value={lockEndDate(position.lockEnd)} />
        <Metric label="Status" value={expired ? "Unlocked" : relativeFromNow(position.lockEnd)} highlight={expired ? "ok" : undefined} />
        <Metric label="Multiplier" value={tierMultiplier(position.multiplierBps)} />
        <Metric
          label="Position"
          value={shortAddr(position.pubkey.toBase58(), 4, 4)}
          link={explorerAddressUrl(position.pubkey.toBase58())}
        />
      </div>

      <div className="mt-5 flex flex-wrap gap-2">
        <button
          type="button"
          onClick={onClaim}
          disabled={busy || pending === 0n}
          className="flex-1 rounded-sm border border-neutral-700 px-4 py-2.5 text-[11px] uppercase tracking-[0.22em] text-neutral-100 transition-colors hover:border-neutral-300 hover:text-neutral-50 disabled:cursor-not-allowed disabled:border-neutral-900 disabled:text-neutral-700 sm:flex-none sm:px-6"
        >
          {busy && busyAction === "claim" ? "Claiming…" : "Claim rewards"}
        </button>
        <button
          type="button"
          onClick={onClose}
          disabled={busy || !expired}
          title={expired ? "Withdraw principal and any pending rewards" : "Available when the lock period ends"}
          className="flex-1 rounded-sm border border-neutral-700 px-4 py-2.5 text-[11px] uppercase tracking-[0.22em] text-neutral-100 transition-colors hover:border-neutral-300 hover:text-neutral-50 disabled:cursor-not-allowed disabled:border-neutral-900 disabled:text-neutral-700 sm:flex-none sm:px-6"
        >
          {busy && busyAction === "close" ? "Closing…" : "Withdraw principal"}
        </button>
      </div>
    </div>
  );
}

function Metric({
  label,
  value,
  link,
  highlight,
}: {
  label: string;
  value: string;
  link?: string;
  highlight?: "ok" | "warn";
}) {
  const tone =
    highlight === "ok" ? "text-emerald-300" : highlight === "warn" ? "text-amber-300" : "text-neutral-100";
  const inner = (
    <>
      <div className="text-[10px] uppercase tracking-[0.2em] text-neutral-600">{label}</div>
      <div className={`mt-1 font-mono text-[12px] ${tone}`}>{value}</div>
    </>
  );
  if (link) {
    return (
      <a href={link} target="_blank" rel="noopener noreferrer" className="block transition-colors hover:opacity-80">
        {inner}
      </a>
    );
  }
  return <div>{inner}</div>;
}

function PositionSkeleton() {
  return (
    <div className="mt-6 space-y-3">
      {[0, 1].map((i) => (
        <div
          key={i}
          className="h-32 animate-pulse rounded-md border border-neutral-900 bg-neutral-950/40"
        />
      ))}
    </div>
  );
}
