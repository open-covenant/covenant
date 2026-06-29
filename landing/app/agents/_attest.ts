// Covenant validation records, shared by the passport (/api/agents/[asset]) and
// the "Covenant Verified" check (/api/agents/[asset]/verify). A record is an MPL
// Core AppData plugin whose on-chain data_authority is the Covenant validator;
// MPL Core enforces that only that key can write it, so authorship is a chain
// fact. The check is a pure function over public DAS output — no Covenant infra
// in the trust path. On-chain keys are camelCase; Helius re-cases to snake_case,
// so every field is read either way.

import {
  ATTESTATION_HASH_ALG,
  ATTESTATION_SCHEMA_V2,
  ATTESTATION_TYPE,
} from "@/app/agents/_registry";

export type Verdict = {
  asset: string | null;
  verified: boolean;
  subjectAsset: string | null;
  authority: string | null;
  responseHash: string | null;
  recordedAt: number | null;
  reasons: string[];
};

export type Accountability = {
  accountable: boolean;
  count: number;
  latest: Verdict | null;
};

const str = (v: unknown): string | undefined => (typeof v === "string" ? v : undefined);
const field = (d: Record<string, unknown>, snake: string, camel: string): string | undefined =>
  str(d[snake]) ?? str(d[camel]);
const isHex64 = (s: string) => /^[0-9a-f]{64}$/.test(s);

export function appData(asset: Record<string, unknown>): Record<string, unknown> | null {
  const plugins = (asset["external_plugins"] ?? []) as Array<Record<string, unknown>>;
  return plugins.find((p) => p["type"] === "AppData") ?? null;
}

/** Pure verification of one DAS asset as a Covenant validation record. */
export function verifyAttestation(asset: Record<string, unknown>, authority: string): Verdict {
  const id = str(asset["id"]) ?? null;
  const plugin = appData(asset);
  if (!plugin) {
    return {
      asset: id,
      verified: false,
      subjectAsset: null,
      authority: null,
      responseHash: null,
      recordedAt: null,
      reasons: ["no AppData external plugin on this asset"],
    };
  }
  const data = (plugin["data"] ?? {}) as Record<string, unknown>;
  const dataAuthority =
    str((plugin["authority"] as Record<string, unknown> | undefined)?.["address"]) ?? null;
  const reasons: string[] = [];

  if (field(data, "type", "type") !== ATTESTATION_TYPE) reasons.push("type is not the ERC-8004 validation type");
  if (field(data, "schema", "schema") !== ATTESTATION_SCHEMA_V2) reasons.push(`schema is not ${ATTESTATION_SCHEMA_V2}`);
  if (field(data, "hash_alg", "hashAlg") !== ATTESTATION_HASH_ALG) reasons.push(`hashAlg is not ${ATTESTATION_HASH_ALG}`);

  const responseHash = field(data, "response_hash", "responseHash") ?? null;
  if (!responseHash) reasons.push("responseHash missing");
  else if (!isHex64(responseHash)) reasons.push("responseHash is not 64 lowercase hex");

  if (!dataAuthority) reasons.push("AppData has no write authority");
  else if (dataAuthority !== authority) reasons.push(`data authority ${dataAuthority} is not the Covenant authority ${authority}`);

  if (field(data, "validator", "validator") !== authority) reasons.push("validator field does not match the expected authority");

  const subject = (data["subject"] ?? {}) as Record<string, unknown>;
  const subjectAsset = field(subject, "asset", "asset") ?? null;
  const recordedRaw = data["recorded_at"] ?? data["recordedAt"];
  const recordedAt = typeof recordedRaw === "number" ? recordedRaw : null;

  return {
    asset: id,
    verified: reasons.length === 0,
    subjectAsset,
    authority: dataAuthority,
    responseHash,
    recordedAt,
    reasons,
  };
}

type Rpc = (method: string, params: unknown) => Promise<unknown>;

/** An agent is accountable iff the Covenant validator has minted a verified
 *  record whose subject is this agent. Pages DAS by the validator's owned
 *  assets and matches subject.asset. */
export async function findAccountability(
  rpc: Rpc,
  agent: string,
  authority: string,
): Promise<Accountability> {
  const verified: Verdict[] = [];
  for (let page = 1; page <= 5; page += 1) {
    const resp = (await rpc("getAssetsByOwner", {
      ownerAddress: authority,
      page,
      limit: 1000,
    })) as Record<string, unknown>;
    const items = (resp["items"] ?? []) as Array<Record<string, unknown>>;
    if (items.length === 0) break;
    for (const item of items) {
      const v = verifyAttestation(item, authority);
      if (v.verified && v.subjectAsset === agent) verified.push(v);
    }
    if (items.length < 1000) break;
  }
  const latest = verified.reduce<Verdict | null>(
    (acc, v) => (acc && (acc.recordedAt ?? 0) >= (v.recordedAt ?? 0) ? acc : v),
    null,
  );
  return { accountable: verified.length > 0, count: verified.length, latest };
}
