"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { useAppKitAccount, useAppKitProvider } from "@reown/appkit/react";
import { type Provider } from "@reown/appkit-adapter-solana/react";
import { PublicKey, Transaction } from "@solana/web3.js";
import { ConnectButton } from "../stake/ConnectButton";
import { getReadConnection } from "../../lib/stake/env";
import {
  buildClaimIx,
  buildClosePositionIx,
} from "../../lib/stake/txBuilder";
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
  tierLabel,
} from "../../lib/stake/format";
import { explorerTxUrl } from "../../lib/stake/env";

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
    setPositions(p.sort((a, b) => Number(a.nonce - b.nonce)));
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
      const { blockhash, lastValidBlockHeight } =
        await connection.getLatestBlockhash("confirmed");
      const transaction = new Transaction({
        feePayer: publicKey,
        recentBlockhash: blockhash,
      });
      transaction.add(
        buildClosePositionIx({ owner: publicKey, nonce: pos.nonce }),
      );
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

  return (
    <div className="mx-auto max-w-3xl">
      <div className="flex items-center justify-between">
        <h1 className="text-3xl font-extralight tracking-tight text-neutral-50 sm:text-4xl">
          Your positions
        </h1>
        <Link
          href="/stake"
          className="text-[11px] uppercase tracking-[0.2em] text-neutral-400 transition-colors hover:text-neutral-100"
        >
          + lock more
        </Link>
      </div>

      <div className="mt-6">
        <ConnectButton />
      </div>

      {!publicKey && (
        <p className="mt-12 text-center text-sm text-neutral-500">
          connect a wallet to view your positions
        </p>
      )}

      {publicKey && positions === null && loading && (
        <p className="mt-12 text-center text-sm text-neutral-500">loading…</p>
      )}

      {publicKey && positions && positions.length === 0 && (
        <div className="mt-12 rounded-md border border-neutral-800 bg-neutral-950/60 p-8 text-center">
          <p className="text-sm text-neutral-400">no positions yet</p>
          <Link
            href="/stake"
            className="mt-4 inline-block rounded-sm border border-neutral-200 bg-neutral-100 px-6 py-2 text-[11px] uppercase tracking-[0.28em] text-neutral-950 hover:bg-white"
          >
            lock $CVNT
          </Link>
        </div>
      )}

      {publicKey && positions && positions.length > 0 && (
        <div className="mt-8 space-y-3">
          {positions.map((pos) => {
            const pending = config
              ? computePendingLamports(pos, config.accSolPerWeight)
              : 0n;
            const now = BigInt(Math.floor(Date.now() / 1000));
            const expired = pos.lockEnd <= now;
            const busy =
              tx.kind === "submitting" &&
              tx.positionPubkey === pos.pubkey.toBase58();
            return (
              <div
                key={pos.pubkey.toBase58()}
                className="rounded-md border border-neutral-800 bg-neutral-950/60 p-5"
              >
                <div className="flex flex-wrap items-start justify-between gap-4">
                  <div>
                    <div className="font-mono text-2xl text-neutral-50">
                      {formatCvnt(pos.amount)} CVNT
                    </div>
                    <div className="mt-1 flex flex-wrap items-center gap-3 text-[11px] uppercase tracking-[0.18em] text-neutral-500">
                      <span>{tierLabel(pos.multiplierBps)}</span>
                      <span>
                        unlock {lockEndDate(pos.lockEnd)} ·{" "}
                        {relativeFromNow(pos.lockEnd)}
                      </span>
                    </div>
                  </div>
                  <div className="text-right">
                    <div className="text-[10px] uppercase tracking-[0.18em] text-neutral-500">
                      claimable
                    </div>
                    <div className="font-mono text-lg text-emerald-300">
                      {formatSol(pending, { maxFrac: 6 })} SOL
                    </div>
                  </div>
                </div>

                <div className="mt-5 flex gap-2">
                  <button
                    type="button"
                    onClick={() => handleClaim(pos)}
                    disabled={busy || pending === 0n}
                    className="flex-1 rounded-sm border border-neutral-700 px-4 py-2 text-[11px] uppercase tracking-[0.2em] text-neutral-200 transition-colors hover:border-neutral-400 hover:text-neutral-50 disabled:cursor-not-allowed disabled:border-neutral-900 disabled:text-neutral-700"
                  >
                    {busy && tx.kind === "submitting" && tx.action === "claim"
                      ? "claiming…"
                      : "claim SOL"}
                  </button>
                  <button
                    type="button"
                    onClick={() => handleClose(pos)}
                    disabled={busy || !expired}
                    title={
                      expired
                        ? "close position and return principal"
                        : "available after unlock date"
                    }
                    className="flex-1 rounded-sm border border-neutral-700 px-4 py-2 text-[11px] uppercase tracking-[0.2em] text-neutral-200 transition-colors hover:border-neutral-400 hover:text-neutral-50 disabled:cursor-not-allowed disabled:border-neutral-900 disabled:text-neutral-700"
                  >
                    {busy && tx.kind === "submitting" && tx.action === "close"
                      ? "closing…"
                      : "close"}
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {tx.kind === "ok" && (
        <p className="mt-6 break-all text-center text-[11px] text-emerald-400">
          done.{" "}
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
      {tx.kind === "error" && (
        <p className="mt-6 break-all text-center text-[11px] text-rose-400">
          {tx.message}
        </p>
      )}
    </div>
  );
}
