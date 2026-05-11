import Fastify from 'fastify';
import { Bot } from 'grammy';
import { MOCK_LEADERBOARD, MOCK_TASKS, resolveSolanaNetwork } from '@covenant/sdk';

const app = Fastify({ logger: true, bodyLimit: 32 * 1024 });
const PORT = Number(process.env.TELEGRAM_PORT ?? 8788);
const TELEGRAM_BOT_TOKEN = process.env.TELEGRAM_BOT_TOKEN;
const network = resolveSolanaNetwork();

let botRunning = false;

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

async function maybeStartBot() {
  if (!TELEGRAM_BOT_TOKEN) return null;

  const bot = new Bot(TELEGRAM_BOT_TOKEN);
  bot.command('status', (ctx) => ctx.reply(renderSummary()));
  bot.command('tasks', (ctx) =>
    ctx.reply(
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
