// Which program the lease clients are driving.
//
// The lease meter shipped first as a standalone program and now also lives
// inside the settlement program, where the protocol config fronts it. The two
// builds differ in exactly three places: what the tick instruction is called,
// whether `open_lease` and `delegate_lease` take the config account, and which
// numbers the refusals come back as. Seeds, account layouts and every other
// instruction are the same, so one client drives both.
//
//   TARGET=compute-lease  (default) the standalone program
//   TARGET=settlement                the lease meter inside settlement
//   PROGRAM=<id>                     overrides the address either way
//
// `settlement` has no default address on purpose. The settlement program is
// deployed on mainnet with live balances behind it, and a client that will
// open escrow and drive a meter has no business defaulting to that address.

import { PublicKey } from "@solana/web3.js";

const MAINNET_SETTLEMENT = "3dTtXH7rah8YyWTsAfSg6qC3iqrJXWGEhAx57uUHXZff";

const DIALECTS = {
  "compute-lease": {
    defaultProgramId: "CLSeVNrRi4TpXsXAkAuLh58kGCCAd1w1bj2CcEhTEESd",
    tick: "tick",
    takesConfig: false,
    errors: {
      notEnoughKeys: 3005, // AccountNotEnoughKeys
      notSigner: 3010, // AccountNotSigner
      wrongCoordinator: 2001, // ConstraintHasOne
      meterClosed: 6009,
      leaseStillRunning: 6010,
    },
    leaseResultFile: "lease-er-result.json",
    attackResultFile: "attack-unauthorized-tick-result.json",
  },
  settlement: {
    defaultProgramId: null,
    tick: "tick_lease",
    takesConfig: true,
    errors: {
      notEnoughKeys: 3005,
      notSigner: 3010,
      wrongCoordinator: 6003, // CovenantError::Unauthorized
      meterClosed: 6027,
      leaseStillRunning: 6028,
    },
    leaseResultFile: "lease-er-result-settlement.json",
    attackResultFile: "attack-unauthorized-tick-result-settlement.json",
  },
};

export function resolveDialect(env = process.env) {
  const name = env.TARGET || "compute-lease";
  const dialect = DIALECTS[name];
  if (!dialect) {
    throw new Error(`unknown TARGET "${name}"; expected one of ${Object.keys(DIALECTS).join(", ")}`);
  }
  const address = env.PROGRAM || dialect.defaultProgramId;
  if (!address) {
    throw new Error(`TARGET=${name} has no default address; set PROGRAM to the deployment to drive`);
  }
  if (address === MAINNET_SETTLEMENT) {
    throw new Error(`refusing to run against the mainnet settlement deployment ${MAINNET_SETTLEMENT}`);
  }
  const programId = new PublicKey(address);
  return {
    name,
    programId,
    tick: dialect.tick,
    // Present only where the program reads the protocol pause. `open_lease`
    // and `delegate_lease` take it; nothing else on the lease path does.
    config: dialect.takesConfig
      ? PublicKey.findProgramAddressSync([Buffer.from("config")], programId)[0]
      : null,
    errors: dialect.errors,
    leaseResultFile: dialect.leaseResultFile,
    attackResultFile: dialect.attackResultFile,
  };
}
