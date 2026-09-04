# Covenant Desk

Robinhood stock tokens trade on chain every hour of the week. The shares behind
them trade for six and a half hours a day, five days a week. Covenant Desk
prices that gap and lets you act on it.

It runs on your machine. It reads Robinhood Chain, the Chainlink equity feeds,
the Robinhood issuer quotes, and the Lighter Robinhood Chain perpetual markets,
and it gives you three numbers for any stock token and for any memecoin quoted
in one: the price the chain is charging, the price the token is worth, and the
gap between them in basis points, with the source of the reference price named.

On top of that it will:

- hold conditional orders (limit, stop, take profit, one cancels the other, at
  the next United States open, and orders conditioned on the premium itself)
  and execute them against Uniswap v4 inside a size cap, a slippage cap, and a
  premium cap;
- size a short on the Lighter Robinhood Chain venue that cancels the stock
  exposure carried inside a memecoin quoted in a stock token, so you keep the
  memecoin and drop the stock;
- record the on-chain price against the reference price on an interval and
  write the result to CSV.

Your keys stay in a file on your machine, mode 600, read once at start. The desk
signs a Uniswap swap or a Lighter order and nothing else. It never signs a
transfer. No account is created anywhere, and no server of ours sees your
trades.

## Install

Node 22 or later. The package is not on the public registry yet, so build it
from the repository:

```sh
git clone https://github.com/open-covenant/covenant.git
cd covenant
pnpm install
pnpm --filter @covenant-org/desk build
npm i -g ./apps/desk
```

Then:

```sh
covenant-desk init --acknowledge-restrictions
covenant-desk start
```

Without the global install, run the built entry point directly:
`node apps/desk/dist/surfaces/cli/main.js status`.

`init` writes `~/.config/covenant-desk/config.json`, an empty `keys.env` at mode
600, and the database. It prints where these tokens are not offered, and it will
not let you turn on signed execution until you have acknowledged that notice.

`start` runs the desk in the background on 127.0.0.1:46631 and prints a link to
the page it serves. `covenant-desk status` shows what it is doing;
`covenant-desk stop` stops it.

To keep it running across logins:

```sh
covenant-desk service install --launchd    # macOS
covenant-desk service install --systemd    # Linux
```

Both write a user service file and print the one command that loads it.

To sign transactions, put an EVM private key in `keys.env` as
`DESK_EVM_PRIVATE_KEY`, set `"live": true` in `config.json`, and create orders
with `--live`. All three are required, and the account needs ETH on chain 4663
for gas.

## Connect your agent

The desk speaks MCP over stdio. The MCP process talks to your running desk over
its loopback API, so start the desk first.

Claude Code:

```sh
claude mcp add covenant-desk -- covenant-desk mcp
```

Hermes, and any other client that reads a JSON server list:

```json
{
  "mcpServers": {
    "covenant-desk": {
      "command": "covenant-desk",
      "args": ["mcp"]
    }
  }
}
```

Codex, in `~/.codex/config.toml`:

```toml
[mcp_servers.covenant-desk]
command = "covenant-desk"
args = ["mcp"]
```

Eleven tools are exposed: `desk_status`, `desk_quote`, `desk_premium`,
`desk_pools`, `desk_order_create`, `desk_orders`, `desk_order_cancel`,
`desk_hedge_plan`, `desk_hedge_apply`, `desk_hedge_status`, and
`desk_recorder_recent`. Each description states its units and its defaults. If
no desk is running, the tools say so instead of starting one.

## Commands

| Command | What it does |
| --- | --- |
| `init [--acknowledge-restrictions]` | Create the config file, the key file, and the database. |
| `start [--foreground]` | Run the desk: price loops, order engine, API, and page. |
| `stop` | Stop a running desk. |
| `status` | Session state, block height, open orders, dry run or live. |
| `quote <token> [--usd 100] [--side buy]` | Price a token on chain, at fair value, and for the size you asked for, with its stock leg premium. |
| `premium [--top 20]` | Rank stock tokens by the gap to their reference price. |
| `pools [--stock NVDA]` | List pools for a stock token, deepest first, with fees. |
| `order create\|list\|show\|cancel` | Work with conditional orders and read the fills they produced. |
| `hedge plan\|apply\|status\|unwind` | Size and hold a short against the stock leg of a position. |
| `record start\|stop\|export --out premium.csv` | Record prices against reference and write CSV. |
| `mcp` | Serve the desk tools over MCP on stdio. |
| `service install\|uninstall` | Install or remove the background service. |

Add `--json` to any read command for the raw answer, and `--help` after any
command for its flags, units, and defaults.

The page at `http://127.0.0.1:46631/` shows the same numbers: session state and
block height, the premium table refreshing every 15 seconds, a quote panel, open
orders with a cancel button, the hedge panel, and 24 hours of recorded premium
for a symbol you pick.

## Safety defaults

- **Dry run.** Every order is a dry run until three things are true at once:
  the jurisdiction notice is acknowledged, `"live": true` is set in the config
  file, and the order was created with `live`. A dry run quotes with the real
  quoter, applies the same limits, and records the fill it would have produced,
  which `order show` prints.
- **Size caps.** 250 USD per order and 1,000 USD over a rolling 24 hours, both
  in the config file. A dry run counts against the daily cap, so you see the
  limit before the desk ever signs anything.
- **Slippage cap.** 100 basis points between the quote and the minimum amount
  the swap will accept.
- **Buy premium cap.** 500 basis points. A buy is refused when the stock leg is
  richer than that.
- **Pool fees.** Chain 4663 carries pools charging up to 90 percent. Any pool
  charging more than 300 basis points is flagged and left out of routing.
- Every refusal names the limit and the number that tripped it.
- The API binds to 127.0.0.1 and requires the bearer token from your config file
  on every route except the health check.

## Limits

- **Where these tokens are offered.** Robinhood stock tokens are issued by
  Robinhood Assets (Jersey) Ltd and are not offered to residents of the United
  States, Canada, the United Kingdom, or Switzerland. Check the rules that apply
  to you before you trade.
- **Reference prices go stale.** The Chainlink equity feeds publish on the
  United States equities calendar and hold the last price when it is closed. The
  desk then uses the Lighter Robinhood Chain perpetual mark, which prices around
  the clock, or the issuer ask. A name with no perpetual market and no fresh
  issuer quote falls back to the last published Chainlink price, labelled
  `chainlink-stale`. Treat those rows as a last close rather than a live price.
- **Hedging needs an account.** Hedge sizing, marks, and funding read without
  credentials. Sending an order needs a funded sub-account on the Lighter
  Robinhood Chain venue and its key in `keys.env`. Without one, `hedge apply`
  returns the plan and the reason it was not sent.
- **One chain.** Chain 4663 only. No custody, no hosted keeper, no bridge.
- **First start takes about ten minutes.** The desk reads every Uniswap v4 pool
  on chain 4663 that holds a stock token, which today is more than fifty
  thousand of them. It records how far it reached, so a restart carries on
  rather than beginning again, and later passes cost one query. Prices fill in
  as the scan advances, deepest pools first.

## License

Apache-2.0. Part of [Covenant](https://opencovenant.org).
