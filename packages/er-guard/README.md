# @covenant-org/er-guard

Session-reliability keeper for [MagicBlock](https://www.magicblock.xyz)
ephemeral rollups. Watches delegated accounts and, while the validator is
healthy, cooperatively undelegates them back to L1 on idle, max-lifetime, or a
soft stall — so the hard-recovery path should never have to fire. Proven on
mainnet against the Covenant settlement program, including a real recovery of a
stuck delegated account.

```
npm install @covenant-org/er-guard
```

## CLI

```
# Covenant mode: watch each owner's credit PDA, undelegate cooperatively
KEYPAIRS=~/.config/solana/id.json ER=https://as.magicblock.app er-guard

# watch mode: any delegated accounts, detect + report
ACCOUNTS=<pubkey,pubkey> er-guard

DRY_RUN=1 ONCE=1 er-guard    # single decide-and-log pass
```

`L1` sets the Solana RPC (defaults to public mainnet). `ER` pointing at a
`*tee*` host mints and refreshes the self-serve JWT automatically. Policy via
`IDLE_MS`, `MAX_LIFETIME_MS`, `STALL_PROBES`, `POLL_MS`, `RETRY_MS`. A quiet run
means nothing needed action; the guard only logs a delegation, a state change,
or a recovery.

## Library

```js
import { guard } from "@covenant-org/er-guard";

await guard({
  l1, erUrl,
  accounts: [{
    address,                      // the delegated PDA
    isActive: (erAccountInfo) => …,   // value that changes while the session is live
    undelegate: (er) => …,            // your program's cooperative undelegate, owner-signed
    requestUndelegate: (l1) => …,     // your program's permissionless request (see below)
  }],
  policy: { idleMs, maxLifetimeMs, stallProbes },
});
```

## The validator-down path (what dlp 3.1.0 actually requires)

`ephemeral-rollups-sdk` v0.16.0 (2026-07-15) upgraded to dlp 3.1.0, which
exposes `RequestUndelegation`: request undelegation on L1, and after the
timeout window the state unlocks permissionlessly at the last committed state.

The part integrators need to know: **the delegated account must sign the
request**, and it is required to be an off-curve, delegation-program-owned PDA
— so only the *owner program* can issue it, by CPI with the PDA's seeds
(`dlp::instruction_builder::request_undelegation`, and
`undelegate_with_rollback_after_timeout` to finalize). A wallet or keeper
cannot send it directly. If you own an ER program and want the permissionless
escape hatch, add a thin instruction that CPIs `request_undelegation` for your
PDA, and wire it into `requestUndelegate` here. Without it the guard still
detects the stall and reports precisely what is stuck — and either way funds
and state stay safe at the last L1 commit; a validator outage is a liveness
problem, not a safety one.

The next planned deploy of Covenant's settlement program adds this instruction;
until then the guard's Covenant mode runs detect-and-report on the
validator-down path, like everyone else's.

Part of the [Covenant × MagicBlock integration](https://github.com/open-covenant/covenant/blob/main/docs/magicblock-integration.md).
