// The Covenant audit gate: a Core Oracle external plugin whose base_address is
// the covenant-oracle ["oracle", asset] PDA. The plugin makes MPL Core read the
// live verdict from that PDA during the agent's lifecycle events, so transfer
// (and, extended, execute) only succeeds while the audit is in policy. The
// gating program is source-verified on mainnet; pinning base_address to the
// derived PDA under COVENANT_ORACLE_PROGRAM is what ties this gate to it.

import { PublicKey } from "@solana/web3.js";
import { COVENANT_ORACLE_PROGRAM } from "@/app/agents/_registry";

export type AuditGate = {
  /** A Covenant oracle plugin gates this asset (base_address is our PDA). */
  gated: boolean;
  programId: string;
  oraclePda: string;
  /** Lifecycle events the oracle can veto, e.g. ["transfer"]. */
  gatedEvents: string[];
  /** true = in policy (transferable), false = out of policy (vetoed), null = unread. */
  inPolicy: boolean | null;
  /** Gate is pinned to the gating program by programId; its source-verified build
   *  status is checkable live (osecVerifyUrl). We pin per-request, not re-verify. */
  programPinned: boolean;
};

type Rpc = (method: string, params: unknown) => Promise<unknown>;

export async function readAuditGate(
  rpc: Rpc,
  externalPlugins: Array<Record<string, unknown>>,
  assetPk: PublicKey,
): Promise<AuditGate | null> {
  const oracle = externalPlugins.find((p) => p["type"] === "Oracle");
  if (!oracle) return null;

  const cfg = ((oracle["adapter_config"] ?? oracle["adapterConfig"]) ?? {}) as Record<string, unknown>;
  const baseAddress = ((cfg["base_address"] ?? cfg["baseAddress"]) as string | undefined) ?? null;
  const [pda] = PublicKey.findProgramAddressSync(
    [Buffer.from("oracle"), assetPk.toBytes()],
    new PublicKey(COVENANT_ORACLE_PROGRAM),
  );
  const gated = baseAddress === pda.toBase58();
  const gatedEvents = Object.keys(
    ((oracle["lifecycle_checks"] ?? oracle["lifecycleChecks"]) ?? {}) as Record<string, unknown>,
  );

  let inPolicy: boolean | null = null;
  if (gated) {
    try {
      const info = (await rpc("getAccountInfo", [
        pda.toBase58(),
        { encoding: "base64" },
      ])) as { value: { data: [string, string]; owner: string } | null } | null;
      // Only trust bytes from an account the oracle program actually owns; a
      // PDA collision under another program would otherwise be read as a verdict.
      if (info?.value?.owner === COVENANT_ORACLE_PROGRAM && info.value.data?.[0]) {
        const buf = Buffer.from(info.value.data[0], "base64");
        // OracleState = disc(8) + OracleValidation::V1 { tag(1), create, transfer, burn, update }.
        // Byte 8 is the enum tag (1 = V1); the transfer verdict is byte 10: 2 = Pass, 1 = Rejected.
        if (buf.length >= 11 && buf[8] === 1) {
          inPolicy = buf[10] === 2 ? true : buf[10] === 1 ? false : null;
        }
      }
    } catch {
      // leave inPolicy null so the gate renders an "unknown verdict" state
    }
  }

  return {
    gated,
    programId: COVENANT_ORACLE_PROGRAM,
    oraclePda: pda.toBase58(),
    gatedEvents,
    inPolicy,
    programPinned: gated,
  };
}
