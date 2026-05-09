import Fastify from 'fastify';
import { Bot } from 'grammy';
import { MOCK_LEADERBOARD, MOCK_TASKS, resolveSolanaNetwork } from '@covenant/sdk';

const app = Fastify({ logger: true });
const PORT = Number(process.env.TELEGRAM_PORT ?? 8788);
const TELEGRAM_BOT_TOKEN = process.env.TELEGRAM_BOT_TOKEN;
const network = resolveSolanaNetwork();

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

  await bot.init();
  if (!process.env.TELEGRAM_WEBHOOK_URL) {
    void bot.start({ drop_pending_updates: true });
  }
  return bot;
}

app.get('/healthz', async () => ({
  ok: true,
  cluster: network.cluster,
  bot_configured: Boolean(TELEGRAM_BOT_TOKEN),
}));

app.get('/summary', async () => ({
  text: renderSummary(),
}));

const isEntry = import.meta.url === `file://${process.argv[1]}`;
if (isEntry) {
  await maybeStartBot();
  await app.listen({ port: PORT, host: '0.0.0.0' });
}
