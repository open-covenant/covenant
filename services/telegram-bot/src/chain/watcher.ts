// Stake announcer: polls the stake program for new `PositionCreated` events
// and pushes a formatted "NEW STAKE" message to a Telegram group.
//
// Why poll instead of `logsSubscribe`: a websocket drops events on reconnect
// and has no backfill, so a redeploy or a flaky socket silently swallows
// stakes. Polling `getSignaturesForAddress` from a persisted cursor survives
// restarts, never double-posts, and self-heals after an outage. Stakes are
// low-volume, so a ~15s poll is cheap and the latency is immaterial.

import { Connection, PublicKey } from "@solana/web3.js";
import type { ConfirmedSignatureInfo } from "@solana/web3.js";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DEFAULT_CVNT_DECIMALS, STAKE_PROGRAM_ID } from "./constants.js";
import {
  extractPositionCreatedEvents,
  type PositionCreatedEvent,
} from "./events.js";
import { fetchStakeSummary, fetchStakeTotals } from "./totals.js";
import type { BotNetwork } from "./network.js";
import { renderNewStake, renderStakeSummary } from "../format.js";

type LogFn = (obj: Record<string, unknown>, msg: string) => void;
export interface WatcherLogger {
  info: LogFn;
  warn: LogFn;
  error: LogFn;
}

export interface StakeWatcherOptions {
  network: BotNetwork;
  /** Posts one announcement to the configured group. Throws → cursor holds. */
  send: (html: string) => Promise<void>;
  log: WatcherLogger;
  pollMs?: number;
  stateDir?: string;
  symbol?: string;
  stakeUrl?: string;
  solscanBase?: string;
  fireUnit?: number;
  /** Telegram custom_emoji_id for the branded bar; falls back to 🔥 when unset. */
  emojiId?: string;
  /** Caption mode for a header-image post: drops the redundant "NEW STAKE" title. */
  bannerMode?: boolean;
  /** How often to post the locked/staked stats summary, in ms. 0 disables it. */
  summaryIntervalMs?: number;
  /** Posts the periodic stats summary (separate channel from per-stake `send`). */
  sendSummary?: (html: string) => Promise<void>;
}

export interface StakeWatcherStatus {
  running: boolean;
  cluster: string;
  mint: string | null;
  pollMs: number;
  lastSignature: string | null;
  lastPollAt: number | null;
  lastAnnouncedAt: number | null;
  announced: number;
  lastSummaryAt: number | null;
  errors: number;
  lastError: string | null;
}

export interface StakeWatcherHandle {
  status(): StakeWatcherStatus;
  stop(): void;
}

const PAGE_LIMIT = 1000;
const MAX_PAGES = 20; // safety bound on a single drain (~20k signatures)

function errMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function resolveStateDir(preferred: string | undefined, log: WatcherLogger): string {
  const candidates = [preferred, "/data/telegram-bot"].filter(
    (value): value is string => Boolean(value),
  );
  for (const dir of candidates) {
    try {
      mkdirSync(dir, { recursive: true });
      return dir;
    } catch {
      // try the next candidate
    }
  }
  const fallback = join(tmpdir(), "covenant-telegram-bot");
  mkdirSync(fallback, { recursive: true });
  log.warn({ fallback }, "stake-watcher:state_dir_fallback");
  return fallback;
}

export function startStakeWatcher(
  options: StakeWatcherOptions,
): StakeWatcherHandle {
  const { network, send, log } = options;
  if (!network.cvntMint) {
    throw new Error("startStakeWatcher requires a configured $CVNT mint");
  }
  // Explicit type so the non-null narrowing survives into the closures below.
  const mint: PublicKey = network.cvntMint;

  const pollMs = options.pollMs && options.pollMs > 0 ? options.pollMs : 15_000;
  const symbol = options.symbol ?? "CVNT";
  const stakeUrl = options.stakeUrl ?? "https://opencovenant.org/stake";
  const solscanBase = options.solscanBase ?? "https://solscan.io";
  const fireUnit = options.fireUnit && options.fireUnit > 0 ? options.fireUnit : 250_000;
  const emojiId = options.emojiId;
  const bannerMode = options.bannerMode;
  const summaryIntervalMs =
    options.summaryIntervalMs && options.summaryIntervalMs > 0
      ? options.summaryIntervalMs
      : 0;
  const sendSummary = options.sendSummary;

  const connection = new Connection(network.rpcUrl, "confirmed");
  const stateDir = resolveStateDir(options.stateDir, log);
  const cursorPath = join(stateDir, "stake-cursor.json");
  const summaryPath = join(stateDir, "stake-summary.json");

  let decimals = DEFAULT_CVNT_DECIMALS;
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const status: StakeWatcherStatus = {
    running: true,
    cluster: network.cluster,
    mint: mint.toBase58(),
    pollMs,
    lastSignature: null,
    lastPollAt: null,
    lastAnnouncedAt: null,
    announced: 0,
    lastSummaryAt: null,
    errors: 0,
    lastError: null,
  };

  function readCursor(): string | null {
    try {
      if (!existsSync(cursorPath)) return null;
      const parsed: unknown = JSON.parse(readFileSync(cursorPath, "utf8"));
      if (
        parsed &&
        typeof parsed === "object" &&
        typeof (parsed as { signature?: unknown }).signature === "string"
      ) {
        return (parsed as { signature: string }).signature;
      }
    } catch (error) {
      log.warn({ err: errMessage(error) }, "stake-watcher:cursor_read_failed");
    }
    return null;
  }

  function writeCursor(signature: string): void {
    try {
      writeFileSync(
        cursorPath,
        JSON.stringify({ signature, updatedAt: Date.now() }),
      );
    } catch (error) {
      log.error(
        { err: errMessage(error) },
        "stake-watcher:cursor_persist_failed",
      );
    }
  }

  function readSummaryAt(): number | null {
    try {
      if (!existsSync(summaryPath)) return null;
      const parsed: unknown = JSON.parse(readFileSync(summaryPath, "utf8"));
      const at = (parsed as { lastSummaryAt?: unknown }).lastSummaryAt;
      return typeof at === "number" ? at : null;
    } catch {
      return null;
    }
  }

  function writeSummaryAt(at: number): void {
    try {
      writeFileSync(summaryPath, JSON.stringify({ lastSummaryAt: at }));
    } catch (error) {
      log.error({ err: errMessage(error) }, "stake-watcher:summary_persist_failed");
    }
  }

  let cursor = readCursor();
  status.lastSignature = cursor;

  // Anchor the summary clock on first boot so the first post lands one full
  // interval out; persisted across redeploys so the cadence never resets.
  const persistedSummaryAt = readSummaryAt();
  let lastSummaryAt: number = persistedSummaryAt ?? Date.now();
  if (persistedSummaryAt === null) writeSummaryAt(lastSummaryAt);
  status.lastSummaryAt = lastSummaryAt;

  async function drainNewSignatures(
    untilSig: string,
  ): Promise<ConfirmedSignatureInfo[]> {
    const collected: ConfirmedSignatureInfo[] = [];
    let before: string | undefined;
    for (let page = 0; page < MAX_PAGES; page += 1) {
      const batch = await connection.getSignaturesForAddress(
        STAKE_PROGRAM_ID,
        { until: untilSig, before, limit: PAGE_LIMIT },
        "confirmed",
      );
      if (batch.length === 0) break;
      collected.push(...batch);
      if (batch.length < PAGE_LIMIT) break;
      before = batch[batch.length - 1]?.signature;
      if (page === MAX_PAGES - 1) {
        log.warn(
          { collected: collected.length },
          "stake-watcher:drain_page_cap",
        );
      }
    }
    // getSignaturesForAddress returns newest→oldest; replay oldest→newest so
    // announcements mirror on-chain order and the cursor advances monotonically.
    return collected.reverse();
  }

  async function eventsForSignature(
    signature: string,
  ): Promise<PositionCreatedEvent[]> {
    const tx = await connection.getTransaction(signature, {
      commitment: "confirmed",
      maxSupportedTransactionVersion: 0,
    });
    return extractPositionCreatedEvents(tx?.meta?.logMessages);
  }

  async function announce(
    event: PositionCreatedEvent,
    signature: string,
  ): Promise<void> {
    let totals: { totalStakedRaw: bigint; pct: number } | null = null;
    try {
      const t = await fetchStakeTotals(connection, mint, network.tokenProgramId);
      totals = { totalStakedRaw: t.totalStakedRaw, pct: t.pct };
      decimals = t.decimals;
    } catch (error) {
      log.warn({ err: errMessage(error) }, "stake-watcher:totals_failed");
    }

    const html = renderNewStake({
      amountRaw: event.amount,
      decimals,
      multiplierBps: event.multiplierBps,
      totals,
      txSignature: signature,
      cluster: network.cluster,
      symbol,
      stakeUrl,
      solscanBase,
      fireUnit,
      emojiId,
      bannerMode,
    });

    // A send failure propagates so the cursor does NOT advance past this
    // signature — the next poll retries it. A create_position tx carries
    // exactly one PositionCreated, so a retry cannot double-post.
    await send(html);
    status.announced += 1;
    status.lastAnnouncedAt = Date.now();
    log.info(
      {
        signature,
        owner: event.owner.toBase58(),
        amount: event.amount.toString(),
        tier_bps: event.multiplierBps,
      },
      "stake-watcher:announced",
    );
  }

  async function maybeSummary(): Promise<void> {
    if (!sendSummary || summaryIntervalMs <= 0) return;
    const now = Date.now();
    if (now - lastSummaryAt < summaryIntervalMs) return;
    // Advance the clock first so a transient failure waits a full interval
    // instead of retrying every poll.
    lastSummaryAt = now;
    writeSummaryAt(now);
    status.lastSummaryAt = now;
    try {
      const s = await fetchStakeSummary(connection, mint, network.tokenProgramId);
      const html = renderStakeSummary({
        lockedRaw: s.lockedRaw,
        stakedRaw: s.stakedRaw,
        decimals: s.decimals,
        combinedPct: s.combinedPct,
        symbol,
        emojiId,
      });
      await sendSummary(html);
      log.info(
        {
          locked: s.lockedRaw.toString(),
          staked: s.stakedRaw.toString(),
          pct: s.combinedPct,
        },
        "stake-watcher:summary_posted",
      );
    } catch (error) {
      log.error({ err: errMessage(error) }, "stake-watcher:summary_failed");
    }
  }

  async function pollOnce(): Promise<void> {
    status.lastPollAt = Date.now();

    if (cursor === null) {
      // Cold start: anchor to the latest signature and do NOT backfill
      // history. The first stake after deploy is announced; the program is
      // always initialized before anyone can stake, so no real stake is the
      // very first signature this anchors on.
      const latest = await connection.getSignaturesForAddress(
        STAKE_PROGRAM_ID,
        { limit: 1 },
        "confirmed",
      );
      const head = latest[0];
      if (head) {
        cursor = head.signature;
        status.lastSignature = cursor;
        writeCursor(cursor);
        log.info({ signature: cursor }, "stake-watcher:cold_start_anchor");
      }
      return;
    }

    const fresh = await drainNewSignatures(cursor);
    for (const info of fresh) {
      if (!info.err) {
        const events = await eventsForSignature(info.signature);
        for (const event of events) {
          await announce(event, info.signature);
        }
      }
      cursor = info.signature;
      status.lastSignature = cursor;
      writeCursor(cursor);
    }
  }

  async function loop(): Promise<void> {
    log.info(
      { cluster: network.cluster, mint: status.mint, pollMs, stateDir },
      "stake-watcher:started",
    );
    while (!stopped) {
      try {
        await pollOnce();
        await maybeSummary();
        status.lastError = null;
      } catch (error) {
        status.errors += 1;
        status.lastError = errMessage(error);
        log.error({ err: status.lastError }, "stake-watcher:poll_error");
      }
      if (stopped) break;
      await new Promise<void>((resolve) => {
        timer = setTimeout(resolve, pollMs);
      });
    }
    status.running = false;
  }

  void loop();

  return {
    status: () => ({ ...status }),
    stop: () => {
      stopped = true;
      if (timer) clearTimeout(timer);
    },
  };
}
