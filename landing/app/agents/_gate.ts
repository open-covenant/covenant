// Structural observations for the Covenant Core Oracle experiment. Matching a
// plugin base address to the derived PDA binds the configured program and asset;
// it does not establish the verdict's semantic correctness or mediate agent
// execution, signing, or payment.

import { PublicKey } from "@solana/web3.js";
import { COVENANT_ORACLE_PROGRAM } from "@/app/agents/_registry";

export type AuditGate = {
  /** The reported Oracle base_address is the configured program's derived PDA. */
  gated: boolean;
  programId: string;
  oraclePda: string;
  /** Lifecycle checks reported in the asset's configured-provider view. */
  gatedEvents: string[];
  /** Decoded experimental transfer verdict; null means unread or unrecognized. */
  inPolicy: boolean | null;
  /** Whether base_address matched the PDA derived under the configured program. */
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
