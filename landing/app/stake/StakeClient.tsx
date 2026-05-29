"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { useAppKitAccount, useAppKitProvider } from "@reown/appkit/react";
import {
  useAppKitConnection,
  type Provider,
} from "@reown/appkit-adapter-solana/react";
import { PublicKey, Transaction } from "@solana/web3.js";
import { ConnectButton } from "./ConnectButton";
import { explorerTxUrl, getClusterConfig } from "../../lib/stake/env";
import {
  TIER_30D_BPS,
  TIER_OPTIONS,
  buildCreatePositionIx,
} from "../../lib/stake/txBuilder";
import {
  fetchConfig,
  fetchOwnerTokenAccountsForMint,
  pickStakeSource,
  sumBalances,
  type ConfigState,
  type OwnedTokenAccount,
} from "../../lib/stake/readers";
import { formatCvnt, parseCvntInput } from "../../lib/stake/format";

type TxState =
  | { phase: "idle" }
  | { phase: "submitting" }
  | { phase: "ok"; sig: string }
  | { phase: "error"; message: string };

export function StakeClient() {
  const { connection } = useAppKitConnection();
  const { address } = useAppKitAccount();
  const { walletProvider } = useAppKitProvider<Provider>("solana");
  const cluster = getClusterConfig();

  const publicKey = useMemo(
    () => (address ? new PublicKey(address) : null),
    [address],
  );

  const [config, setConfig] = useState<ConfigState | null>(null);
  const [accounts, setAccounts] = useState<OwnedTokenAccount[] | null>(null);
  const [amount, setAmount] = useState("");
  const [tierBps, setTierBps] = useState(TIER_30D_BPS);
  const [tx, setTx] = useState<TxState>({ phase: "idle" });

  const balance = accounts === null ? null : sumBalances(accounts);
  const stakeSource = accounts === null ? null : pickStakeSource(accounts);

  useEffect(() => {
    if (!connection) return;
    let cancelled = false;
    fetchConfig(connection).then((c) => {
      if (!cancelled) setConfig(c);
    });
    return () => {
      cancelled = true;
    };
  }, [connection]);

  useEffect(() => {
    if (!publicKey || !connection) {
      setAccounts(null);
      return;
    }
    let cancelled = false;
    fetchOwnerTokenAccountsForMint(
      connection,
      publicKey,
      cluster.cvntMint,
      cluster.tokenProgramId,
    ).then((accs) => {
      if (!cancelled) setAccounts(accs);
    }).catch(() => {
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
  const canSubmit =
    !!publicKey &&
    !!config &&
    !!stakeSource &&
    meetsMin &&
    sufficientInSource &&
    tx.phase !== "submitting";

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

  return (
    <div className="mx-auto max-w-xl">
      <h1 className="text-3xl font-extralight tracking-tight text-neutral-50 sm:text-4xl">
        Lock $CVNT, earn SOL
      </h1>
      <p className="mt-3 text-sm leading-relaxed text-neutral-400">
        Real-yield staking sourced from pump.fun creator fees on the $CVNT
        post-graduation pool. Lock for 30, 90, 180, or 365 days at fixed
        weight multipliers. No emissions, no inflation, no APR promise —
        rewards are whatever the protocol actually distributes.
      </p>

      <div className="mt-10 flex items-center justify-between gap-4">
        <ConnectButton />
        {publicKey && balance !== null && (
          <div className="text-right">
            <div className="text-[10px] uppercase tracking-[0.18em] text-neutral-500">
              wallet balance
            </div>
            <div className="font-mono text-sm text-neutral-200">
              {formatCvnt(balance)} CVNT
            </div>
          </div>
        )}
      </div>

      <div className="mt-8 rounded-md border border-neutral-800 bg-neutral-950/60 p-6">
        <label className="block">
          <span className="text-[10px] uppercase tracking-[0.18em] text-neutral-500">
            Amount
          </span>
          <input
            type="text"
            inputMode="decimal"
            placeholder="0"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
            className="mt-2 w-full border-b border-neutral-700 bg-transparent pb-2 text-3xl font-extralight text-neutral-50 outline-none focus:border-neutral-300"
          />
          <div className="mt-2 flex items-center justify-between text-[11px] text-neutral-500">
            <span>
              min {config ? formatCvnt(config.minLockAmount, { maxFrac: 0 }) : "—"} CVNT
            </span>
            {balance !== null && (
              <button
                type="button"
                onClick={() => setAmount(formatCvnt(balance, { maxFrac: 6 }))}
                className="text-neutral-400 hover:text-neutral-100"
              >
                max
              </button>
            )}
          </div>
        </label>

        <div className="mt-8">
          <div className="text-[10px] uppercase tracking-[0.18em] text-neutral-500">
            Lock period
          </div>
          <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-4">
            {TIER_OPTIONS.map((opt) => (
              <button
                key={opt.bps}
                type="button"
                onClick={() => setTierBps(opt.bps)}
                className={`rounded-sm border px-3 py-3 text-left text-[12px] font-mono transition-colors ${
                  tierBps === opt.bps
                    ? "border-neutral-200 bg-neutral-100 text-neutral-950"
                    : "border-neutral-800 bg-neutral-950/30 text-neutral-300 hover:border-neutral-600"
                }`}
              >
                <div className="text-sm font-medium">{opt.days}d</div>
                <div className="text-[10px] uppercase tracking-[0.18em] opacity-70">
                  {(opt.bps / 10_000).toFixed(1)}x
                </div>
              </button>
            ))}
          </div>
        </div>

        <button
          type="button"
          onClick={handleStake}
          disabled={!canSubmit}
          className="mt-8 w-full rounded-sm border border-neutral-200 bg-neutral-100 px-6 py-3 text-[12px] uppercase tracking-[0.28em] text-neutral-950 transition-colors hover:bg-white disabled:cursor-not-allowed disabled:border-neutral-800 disabled:bg-transparent disabled:text-neutral-600"
        >
          {tx.phase === "submitting" ? "submitting…" : "lock"}
        </button>

        {!publicKey && (
          <p className="mt-3 text-center text-[11px] text-neutral-500">
            connect a wallet to lock
          </p>
        )}
        {publicKey && parsedAmount !== null && !meetsMin && (
          <p className="mt-3 text-center text-[11px] text-amber-400">
            below minimum lock amount
          </p>
        )}
        {publicKey && parsedAmount !== null && meetsMin && !sufficientInSource && stakeSource && (
          <p className="mt-3 text-center text-[11px] text-amber-400">
            largest CVNT account holds {formatCvnt(stakeSource.amount)} — lock
            from there, or consolidate
          </p>
        )}
        {publicKey && accounts !== null && accounts.length === 0 && (
          <p className="mt-3 text-center text-[11px] text-amber-400">
            no $CVNT in this wallet
          </p>
        )}
        {publicKey && accounts !== null && accounts.length > 1 && (
          <p className="mt-3 text-center text-[11px] text-neutral-500">
            {accounts.length} CVNT accounts found — locking from the largest
            ({formatCvnt(stakeSource?.amount ?? 0n)})
          </p>
        )}

        {tx.phase === "ok" && (
          <p className="mt-4 break-all text-center text-[11px] text-emerald-400">
            locked.{" "}
            <a
              href={explorerTxUrl(tx.sig)}
              target="_blank"
              rel="noopener noreferrer"
              className="underline"
            >
              view tx
            </a>
          </p>
        )}
        {tx.phase === "error" && (
          <p className="mt-4 break-all text-center text-[11px] text-rose-400">
            {tx.message}
          </p>
        )}
      </div>

      <div className="mt-6 flex items-center justify-between text-[11px] text-neutral-500">
        <Link href="/positions" className="hover:text-neutral-100">
          your positions →
        </Link>
        <Link href="/treasury" className="hover:text-neutral-100">
          treasury →
        </Link>
      </div>
    </div>
  );
}
