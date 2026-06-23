import Fastify from 'fastify';
import { Bot, type Context, type NextFunction } from 'grammy';
import { MOCK_LEADERBOARD, MOCK_TASKS, resolveSolanaNetwork } from '@covenant/sdk';

const app = Fastify({ logger: true, bodyLimit: 32 * 1024 });
const PORT = Number(process.env.TELEGRAM_PORT ?? 8788);
const TELEGRAM_BOT_TOKEN = process.env.TELEGRAM_BOT_TOKEN;
const network = resolveSolanaNetwork();
const RATE_LIMIT_PER_MIN = Number(process.env.TELEGRAM_RATE_LIMIT_PER_MIN ?? 5);

let botRunning = false;

// Telegram numeric user-ids permitted to invoke any bot command. Empty set
// means deny-all — commands silently log + drop, no reply to the caller so
// an attacker probing the bot can't enumerate allowed accounts.
export function parseAllowlist(raw: string | undefined): Set<number> {
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

app.get('/healthz', async () => ({
  ok: true,
  cluster: network.cluster,
  bot_configured: Boolean(TELEGRAM_BOT_TOKEN),
  bot_running: botRunning,
  allowlist_size: ALLOWED_USERS.size,
  rate_limit_per_min: RATE_LIMIT_PER_MIN,
}));

app.get('/summary', async () => ({
  text: renderSummary(),
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
  await maybeStartBot();
  await app.listen({ port: PORT, host: '0.0.0.0' });
}
