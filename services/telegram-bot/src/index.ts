import Fastify from 'fastify';
import { Bot, type Context, type NextFunction } from 'grammy';
import { MOCK_LEADERBOARD, MOCK_TASKS } from './mock.js';
import { resolveBotNetwork } from './chain/network.js';
import { renderNewStake } from './format.js';
import { startStakeWatcher, type StakeWatcherHandle } from './chain/watcher.js';

const app = Fastify({ logger: true, bodyLimit: 32 * 1024 });
// Render injects PORT and health-checks it, so prefer it; TELEGRAM_PORT is a
// local-dev override, 8788 the final fallback.
const PORT = Number(process.env.PORT ?? process.env.TELEGRAM_PORT ?? 8788);
const TELEGRAM_BOT_TOKEN = process.env.TELEGRAM_BOT_TOKEN;
const network = resolveBotNetwork();
const RATE_LIMIT_PER_MIN = Number(process.env.TELEGRAM_RATE_LIMIT_PER_MIN ?? 5);

// Stake announcer config. The announcer posts a "NEW STAKE" message to
// TELEGRAM_ANNOUNCE_CHAT_ID whenever a PositionCreated event lands on-chain.
// It is independent of the command allowlist (it broadcasts, never replies).
const ANNOUNCE_CHAT_ID = process.env.TELEGRAM_ANNOUNCE_CHAT_ID?.trim();
const TOKEN_SYMBOL =
  process.env.COVENANT_TOKEN_SYMBOL ??
  process.env.NEXT_PUBLIC_COVENANT_TOKEN_SYMBOL ??
  'CVNT';
const STAKE_URL =
  process.env.STAKE_ANNOUNCE_STAKE_URL ?? 'https://opencovenant.org/stake';
const SOLSCAN_BASE =
  process.env.STAKE_ANNOUNCE_SOLSCAN_BASE ?? 'https://solscan.io';
const FIRE_UNIT = Number(process.env.STAKE_ANNOUNCE_FIRE_UNIT ?? '250000');
// Telegram custom_emoji_id rendered as the branded bar instead of 🔥. Must be
// from a custom-emoji set this bot owns. Unset → falls back to 🔥.
const ANNOUNCE_EMOJI_ID = process.env.STAKE_ANNOUNCE_EMOJI_ID?.trim() || undefined;
const WATCHER_POLL_MS = Number(process.env.STAKE_WATCHER_POLL_MS ?? '15000');
const WATCHER_STATE_DIR = process.env.STAKE_WATCHER_STATE_DIR;

let botRunning = false;
let watcher: StakeWatcherHandle | null = null;

// Telegram numeric user-ids permitted to invoke any bot command. Empty set
// means deny-all — commands silently log + drop, no reply to the caller so
// an attacker probing the bot can't enumerate allowed accounts.
function parseAllowlist(raw: string | undefined): Set<number> {
  if (!raw) return new Set();
  return new Set(
    raw
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean)
      .map(Number)
      .filter((n) => Number.isInteger(n) && n > 0),
  );
}

const ALLOWED_USERS = parseAllowlist(process.env.TELEGRAM_ALLOWED_USER_IDS);

// Per-user request bucket. Single-process bot, single-instance scope —
// the polling/webhook layer is not horizontally scaled. Pruned on miss.
type Bucket = { count: number; resetAt: number };
const userBuckets = new Map<number, Bucket>();
function rateLimit(userId: number): boolean {
  const now = Date.now();
  const bucket = userBuckets.get(userId);
  if (!bucket || now >= bucket.resetAt) {
    userBuckets.set(userId, { count: 1, resetAt: now + 60_000 });
    return true;
  }
  if (bucket.count >= RATE_LIMIT_PER_MIN) return false;
  bucket.count += 1;
  return true;
}

function renderSummary() {
  const topAgent = MOCK_LEADERBOARD[0];
  return [
    `Covenant/Solana status`,
    `Cluster: ${network.cluster}`,
    `Open tasks: ${MOCK_TASKS.length}`,
    topAgent
      ? `Top agent: ${topAgent.agentId} (${topAgent.score})`
      : 'Top agent: unavailable',
  ].join('\n');
}

// All bot replies route through this helper so an accidental future
// `parse_mode: 'MarkdownV2'` change cannot turn a `_`/`*`/`[` inside a
// task or agent id into formatting or injection. Plain text only.
async function safeReply(ctx: Context, text: string): Promise<void> {
  await ctx.reply(text, { parse_mode: undefined });
}

async function gate(ctx: Context, next: NextFunction): Promise<void> {
  const userId = ctx.from?.id;
  if (typeof userId !== 'number' || !ALLOWED_USERS.has(userId)) {
    app.log.warn(
      {
        telegram_user_id: userId ?? null,
        update_type: ctx.update ? Object.keys(ctx.update).find((k) => k !== 'update_id') : null,
      },
      'telegram-bot:denied_allowlist',
    );
    return;
  }
  if (!rateLimit(userId)) {
    app.log.warn({ telegram_user_id: userId }, 'telegram-bot:rate_limited');
    return;
  }
  await next();
}

async function maybeStartBot() {
  if (!TELEGRAM_BOT_TOKEN) return null;
  if (ALLOWED_USERS.size === 0) {
    app.log.warn(
      'telegram-bot: TELEGRAM_ALLOWED_USER_IDS is empty; all commands will be denied. Set comma-separated numeric user ids to enable specific accounts.',
    );
  }

  const bot = new Bot(TELEGRAM_BOT_TOKEN);
  bot.use(gate);
  bot.command('status', (ctx) => safeReply(ctx, renderSummary()));
  bot.command('tasks', (ctx) =>
    safeReply(
      ctx,
      MOCK_TASKS.map((task) => `${task.taskId} · ${task.status} · ${task.paymentAmount}`).join('\n'),
    ),
  );
  // Renders a sample NEW STAKE message to the caller so an operator can verify
  // formatting without waiting for a real stake. Allowlist-gated like the rest;
  // the content is fully bot-controlled, so HTML parse mode is safe here.
  bot.command('stakepreview', (ctx) =>
    ctx.reply(renderStakePreview(), {
      parse_mode: 'HTML',
      link_preview_options: { is_disabled: true },
    }),
  );
  bot.catch((err) => {
    app.log.error({ err: err.error }, 'telegram-bot:handler_error');
  });

  await bot.init();
  if (!process.env.TELEGRAM_WEBHOOK_URL) {
    bot
      .start({ drop_pending_updates: true })
      .then(() => {
        botRunning = false;
      })
      .catch((err: unknown) => {
        botRunning = false;
        app.log.error({ err: err instanceof Error ? err.message : String(err) }, 'telegram-bot:start_failed');
      });
    botRunning = true;
  }
  return bot;
}

function renderStakePreview(): string {
  // Mirrors the canonical example: a ~9.99M CVNT 7-day lock against a ~201M
  // total at ~20% of supply. Static so the command renders instantly without
  // an RPC round-trip.
  return renderNewStake({
    amountRaw: 9_988_818n * 1_000_000n,
    decimals: 6,
    multiplierBps: 5000,
    totals: { totalStakedRaw: 201_109_469_720_000n, pct: 20.1 },
    txSignature:
      '5uA7rQ9mZQ7tJ4o8h4q9LkT7o6r8mQ2p5z6x7c8v9b1n2m3q4w5e6r7t8y9u1111',
    cluster: network.cluster,
    symbol: TOKEN_SYMBOL,
    stakeUrl: STAKE_URL,
    solscanBase: SOLSCAN_BASE,
    fireUnit: FIRE_UNIT,
    emojiId: ANNOUNCE_EMOJI_ID,
  });
}

function maybeStartAnnouncer(bot: Bot): void {
  if (!ANNOUNCE_CHAT_ID) {
    app.log.warn(
      'telegram-bot: TELEGRAM_ANNOUNCE_CHAT_ID unset; stake announcer disabled',
    );
    return;
  }
  if (!network.cvntMint) {
    app.log.warn(
      'telegram-bot: no $CVNT mint for the active cluster (set COVNT_MINT); stake announcer disabled',
    );
    return;
  }
  const chatId = ANNOUNCE_CHAT_ID;
  watcher = startStakeWatcher({
    network,
    send: async (html) => {
      await bot.api.sendMessage(chatId, html, {
        parse_mode: 'HTML',
        link_preview_options: { is_disabled: true },
      });
    },
    log: {
      info: (obj, msg) => app.log.info(obj, msg),
      warn: (obj, msg) => app.log.warn(obj, msg),
      error: (obj, msg) => app.log.error(obj, msg),
    },
    pollMs: WATCHER_POLL_MS,
    stateDir: WATCHER_STATE_DIR,
    symbol: TOKEN_SYMBOL,
    stakeUrl: STAKE_URL,
    solscanBase: SOLSCAN_BASE,
    fireUnit: FIRE_UNIT,
    emojiId: ANNOUNCE_EMOJI_ID,
  });
  app.log.info(
    {
      chat_id: chatId,
      cluster: network.cluster,
      mint: network.cvntMint.toBase58(),
    },
    'telegram-bot:stake_announcer_started',
  );
}

app.get('/healthz', async () => ({
  ok: true,
  cluster: network.cluster,
  bot_configured: Boolean(TELEGRAM_BOT_TOKEN),
  bot_running: botRunning,
  allowlist_size: ALLOWED_USERS.size,
  rate_limit_per_min: RATE_LIMIT_PER_MIN,
  announcer: {
    configured: Boolean(ANNOUNCE_CHAT_ID && network.cvntMint),
    ...(watcher ? watcher.status() : { running: false }),
  },
}));

app.get('/summary', async () => ({
  text: renderSummary(),
}));

app.get('/announce/preview', async () => ({
  text: renderStakePreview(),
}));

const isEntry = import.meta.url === `file://${process.argv[1]}`;
if (isEntry) {
  process.on('unhandledRejection', (reason) => {
    app.log.error({ err: reason instanceof Error ? reason.message : String(reason) }, 'telegram-bot:unhandled_rejection');
    process.exit(1);
  });
  process.on('uncaughtException', (err) => {
    app.log.error({ err: err.message }, 'telegram-bot:uncaught_exception');
    process.exit(1);
  });
  const startedBot = await maybeStartBot();
  if (startedBot) maybeStartAnnouncer(startedBot);
  await app.listen({ port: PORT, host: '0.0.0.0' });
}
