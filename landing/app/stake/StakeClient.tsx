"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { useAppKitAccount, useAppKitProvider } from "@reown/appkit/react";
import { type Provider } from "@reown/appkit-adapter-solana/react";
import { PublicKey, Transaction } from "@solana/web3.js";
import { ConnectButton } from "./ConnectButton";
import {
  explorerTxUrl,
  getClusterConfig,
  getReadConnection,
} from "../../lib/stake/env";
import {
  TIER_30D_BPS,
  TIER_OPTIONS,
  buildCreatePositionIx,
} from "../../lib/stake/txBuilder";
import {
  fetchConfig,
  fetchOwnerTokenAccountsForMint,
  fetchRewardVaultLamports,
  pickStakeSource,
  sumBalances,
  type ConfigState,
  type OwnedTokenAccount,
} from "../../lib/stake/readers";
import { formatCvnt, formatSol, formatWithGrouping, parseCvntInput, shortAddr } from "../../lib/stake/format";

type TxState =
  | { phase: "idle" }
  | { phase: "submitting" }
  | { phase: "ok"; sig: string }
  | { phase: "error"; message: string };

export function StakeClient() {
  const connection = useMemo(() => getReadConnection(), []);
  const { address } = useAppKitAccount();
  const { walletProvider } = useAppKitProvider<Provider>("solana");
  const cluster = getClusterConfig();

  const publicKey = useMemo(
    () => (address ? new PublicKey(address) : null),
    [address],
  );

  const [config, setConfig] = useState<ConfigState | null>(null);
  const [rewardVaultLamports, setRewardVaultLamports] = useState<bigint | null>(null);
  const [accounts, setAccounts] = useState<OwnedTokenAccount[] | null>(null);
  const [amount, setAmount] = useState("");
  const [tierBps, setTierBps] = useState(TIER_30D_BPS);
  const [tx, setTx] = useState<TxState>({ phase: "idle" });

  const balance = accounts === null ? null : sumBalances(accounts);
  const stakeSource = accounts === null ? null : pickStakeSource(accounts);

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      fetchConfig(connection),
      fetchRewardVaultLamports(connection),
    ]).then(([c, rv]) => {
      if (cancelled) return;
      setConfig(c);
      setRewardVaultLamports(rv);
    });
    return () => {
      cancelled = true;
    };
  }, [connection, tx]);

  useEffect(() => {
    if (!publicKey) {
      setAccounts(null);
      return;
    }
    let cancelled = false;
    fetchOwnerTokenAccountsForMint(
      connection,
      publicKey,
      cluster.cvntMint,
      cluster.tokenProgramId,
    )
      .then((accs) => {
        if (!cancelled) setAccounts(accs);
      })
      .catch(() => {
        if (!cancelled) setAccounts([]);
      });
    return () => {
      cancelled = true;
    };
  }, [connection, publicKey, cluster.cvntMint, cluster.tokenProgramId, tx]);

  const parsedAmount = useMemo(() => parseCvntInput(amount), [amount]);
  const minLockAmount = config?.minLockAmount ?? 0n;
  const meetsMin = parsedAmount !== null && parsedAmount >= minLockAmount;
  const sufficientInSource =
    parsedAmount !== null &&
    stakeSource !== null &&
    stakeSource.amount >= parsedAmount;
  const positionWeight = parsedAmount !== null
    ? (parsedAmount * BigInt(tierBps)) / 10_000n
    : 0n;
  const canSubmit =
    !!publicKey &&
    !!config &&
    !!stakeSource &&
    meetsMin &&
    sufficientInSource &&
    tx.phase !== "submitting";

  const handlePreset = (fraction: number) => {
    if (!stakeSource) return;
    const amt = (stakeSource.amount * BigInt(Math.round(fraction * 1000))) / 1000n;
    setAmount(formatCvnt(amt, { maxFrac: 6 }));
  };

  const handleStake = async () => {
    if (!publicKey || !walletProvider || !parsedAmount || !connection || !stakeSource) return;
    setTx({ phase: "submitting" });
    try {
      const nonce = BigInt(Date.now());
      const ixs = [
        buildCreatePositionIx({
          owner: publicKey,
          ownerCvntAccount: stakeSource.pubkey,
          nonce,
          amount: parsedAmount,
          lockTierBps: tierBps,
        }),
      ];
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
      setTx({ phase: "ok", sig });
      setAmount("");
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      setTx({ phase: "error", message });
    }
  };

  const minLockDisplay = config ? formatCvnt(config.minLockAmount, { maxFrac: 0 }) : "—";
  const balanceDisplay = balance !== null ? formatCvnt(balance, { maxFrac: 2 }) : "—";

  return (
    <div className="mx-auto w-full">
      <PageHeader title="Stake" subtitle="Lock CVNT to earn a share of protocol revenue." />

      <div className="grid grid-cols-1 gap-6 lg:grid-cols-[1.1fr_0.9fr]">
        <Panel>
          <PanelEyebrow>Open position</PanelEyebrow>

          <div className="mt-6">
            <FieldLabel>Amount</FieldLabel>
            <div className="mt-3 flex items-baseline gap-3 border-b border-neutral-800 pb-2 transition-colors focus-within:border-neutral-300">
              <input
                type="text"
                inputMode="decimal"
                placeholder="0"
                value={amount}
                onChange={(e) => setAmount(e.target.value)}
                className="min-w-0 flex-1 bg-transparent text-3xl font-light tracking-tight text-neutral-50 outline-none placeholder:text-neutral-700 sm:text-4xl"
              />
              <span className="text-[11px] uppercase tracking-[0.22em] text-neutral-500">CVNT</span>
            </div>
            <div className="mt-3 flex flex-wrap items-center justify-between gap-3 text-[11px] text-neutral-500">
              <div className="flex items-center gap-4">
                <span>
                  Available <span className="font-mono text-neutral-300">{balanceDisplay}</span>
                </span>
                <span>
                  Minimum <span className="font-mono text-neutral-300">{minLockDisplay}</span>
                </span>
              </div>
              <div className="flex items-center gap-2">
                <PresetButton onClick={() => handlePreset(0.25)} disabled={!stakeSource}>25%</PresetButton>
                <PresetButton onClick={() => handlePreset(0.5)} disabled={!stakeSource}>50%</PresetButton>
                <PresetButton onClick={() => handlePreset(1)} disabled={!stakeSource}>Max</PresetButton>
              </div>
            </div>
          </div>

          <div className="mt-8">
            <FieldLabel>Lock period</FieldLabel>
            <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-4">
              {TIER_OPTIONS.map((opt) => (
                <TierButton
                  key={opt.bps}
                  selected={tierBps === opt.bps}
                  onClick={() => setTierBps(opt.bps)}
                  days={opt.days}
                  multiplier={`${(opt.bps / 10_000).toFixed(2)}×`}
                />
              ))}
            </div>
            <div className="mt-4 grid grid-cols-2 gap-3 border-t border-neutral-900 pt-4 text-[11px] sm:grid-cols-3">
              <Metric label="Multiplier" value={`${(tierBps / 10_000).toFixed(2)}×`} />
              <Metric
                label="Position weight"
                value={parsedAmount !== null ? formatCvnt(positionWeight, { maxFrac: 0 }) : "—"}
              />
              <Metric
                label="Pool share after lock"
                value={(() => {
                  if (!config || parsedAmount === null) return "—";
                  const newTotal = config.totalWeight + positionWeight;
                  if (newTotal === 0n) return "—";
                  const sharePct = Number((positionWeight * 10_000n) / newTotal) / 100;
                  return `${sharePct.toFixed(2)}%`;
                })()}
              />
            </div>
          </div>

          <div className="mt-10">
            <button
              type="button"
              onClick={handleStake}
              disabled={!canSubmit}
              className="w-full rounded-sm bg-neutral-50 px-6 py-4 text-[11px] uppercase tracking-[0.28em] text-neutral-950 transition-colors hover:bg-white disabled:cursor-not-allowed disabled:bg-neutral-900 disabled:text-neutral-600"
            >
              {tx.phase === "submitting" ? "Submitting…" : "Lock position"}
            </button>

            <div className="mt-3 min-h-[1.25rem] text-center text-[11px]">
              {!publicKey && <span className="text-neutral-500">Connect a wallet to open a position.</span>}
              {publicKey && accounts !== null && accounts.length === 0 && (
                <span className="text-amber-300">This wallet holds no CVNT.</span>
              )}
              {publicKey && parsedAmount !== null && !meetsMin && (
                <span className="text-amber-300">
                  Amount must be at least <span className="font-mono">{minLockDisplay}</span> CVNT.
                </span>
              )}
              {publicKey &&
                parsedAmount !== null &&
                meetsMin &&
                !sufficientInSource &&
                stakeSource && (
                  <span className="text-amber-300">
                    Largest CVNT account holds {formatCvnt(stakeSource.amount, { maxFrac: 2 })} CVNT.
                  </span>
                )}
              {tx.phase === "ok" && (
                <span className="text-emerald-300">
                  Position opened.{" "}
                  <a href={explorerTxUrl(tx.sig)} target="_blank" rel="noopener noreferrer" className="underline">
                    View transaction
                  </a>
                </span>
              )}
              {tx.phase === "error" && (
                <span className="break-all text-rose-300">{tx.message}</span>
              )}
            </div>
          </div>

          {publicKey && stakeSource && accounts && accounts.length > 0 && (
            <div className="mt-8 border-t border-neutral-900 pt-4 text-[10px] uppercase tracking-[0.22em] text-neutral-600">
              Source · <span className="font-mono normal-case tracking-normal text-neutral-500">{shortAddr(stakeSource.pubkey.toBase58(), 6, 6)}</span>
              {accounts.length > 1 && <span className="ml-2">({accounts.length} CVNT accounts found, largest selected)</span>}
            </div>
          )}
        </Panel>

        <div className="flex flex-col gap-6">
          <Panel>
            <PanelEyebrow>Protocol</PanelEyebrow>
            <div className="mt-6 grid grid-cols-1 gap-4">
              <StatRow
                label="Total weight locked"
                value={config ? formatWithGrouping(config.totalWeight) : "—"}
              />
              <StatRow
                label="Lifetime SOL distributed"
                value={
                  config
                    ? `${formatSol(config.cumulativeSolDistributed, { maxFrac: 4 })} SOL`
                    : "—"
                }
              />
              <StatRow
                label="Active positions"
                value={config ? `${config.activeLockCount} / ${config.maxActiveLocks}` : "—"}
              />
              <StatRow
                label="Reward vault"
                value={
                  rewardVaultLamports !== null
                    ? `${formatSol(rewardVaultLamports, { maxFrac: 4 })} SOL`
                    : "—"
                }
              />
            </div>
            <div className="mt-6 flex items-center justify-between border-t border-neutral-900 pt-4 text-[10px] uppercase tracking-[0.22em] text-neutral-600">
              <Link href="/treasury" className="transition-colors hover:text-neutral-300">
                Protocol treasury →
              </Link>
              <Link href="/positions" className="transition-colors hover:text-neutral-300">
                Your positions →
              </Link>
            </div>
          </Panel>

          <Panel>
            <PanelEyebrow>How it works</PanelEyebrow>
            <ol className="mt-6 space-y-5 text-[13px] leading-relaxed text-neutral-400">
              <Step
                index="01"
                title="Lock CVNT for a fixed period"
                body="Choose 30, 90, 180, or 365 days. Each period carries a weight multiplier of 1.0×, 1.5×, 2.0×, or 3.0× respectively."
              />
              <Step
                index="02"
                title="Earn a pro-rata share of distributions"
                body="The protocol routes 25% of its revenue to lockers in SOL. Your share of each distribution equals your position weight divided by the total weight of all open positions."
              />
              <Step
                index="03"
                title="Claim anytime, withdraw at lock end"
                body="Accrued SOL is claimable at any moment. Principal becomes withdrawable when the lock period elapses."
              />
            </ol>
          </Panel>

          <Panel>
            <PanelEyebrow>Considerations</PanelEyebrow>
            <ul className="mt-6 space-y-3 text-[12px] leading-relaxed text-neutral-500">
              <li>Locked principal is non-transferable and cannot be withdrawn before the lock period ends.</li>
              <li>Distribution amounts depend on protocol revenue. Past distributions do not predict future amounts.</li>
              <li>The protocol may be paused by the operator in response to incidents. New positions and reward claims are paused with it; principal withdrawal after lock expiry is not.</li>
            </ul>
          </Panel>
        </div>
      </div>
    </div>
  );
}

function PageHeader({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div className="mb-10 flex items-end justify-between gap-6 border-b border-neutral-900 pb-6">
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

function FieldLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-[10px] uppercase tracking-[0.22em] text-neutral-500">{children}</div>
  );
}

function PresetButton({
  onClick,
  disabled,
  children,
}: {
  onClick: () => void;
  disabled?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="rounded-sm border border-neutral-800 px-2.5 py-1 text-[10px] uppercase tracking-[0.18em] text-neutral-400 transition-colors hover:border-neutral-500 hover:text-neutral-200 disabled:cursor-not-allowed disabled:border-neutral-900 disabled:text-neutral-700"
    >
      {children}
    </button>
  );
}

function TierButton({
  selected,
  onClick,
  days,
  multiplier,
}: {
  selected: boolean;
  onClick: () => void;
  days: number;
  multiplier: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`rounded-sm border px-3 py-4 text-left transition-all ${
        selected
          ? "border-neutral-100 bg-neutral-100 text-neutral-950"
          : "border-neutral-800 bg-neutral-950/30 text-neutral-300 hover:border-neutral-600 hover:bg-neutral-900/40"
      }`}
    >
      <div className="font-mono text-lg leading-none">{days}</div>
      <div className="mt-1 text-[10px] uppercase tracking-[0.2em] opacity-70">days · {multiplier}</div>
    </button>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-[10px] uppercase tracking-[0.2em] text-neutral-600">{label}</div>
      <div className="mt-1 font-mono text-sm text-neutral-200">{value}</div>
    </div>
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
