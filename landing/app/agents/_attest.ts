// Covenant validation-record observations shared by the passport and its JSON
// endpoint. These checks run over a configured DAS provider's response. They
// match the reported record envelope; they do not authenticate the underlying
// Core account, prove the claim, or establish completeness of the record set.
// On-chain keys are camelCase; Helius re-cases to snake_case, so every field is
// read either way.

import {
  ATTESTATION_HASH_ALG,
  ATTESTATION_SCHEMA_V2,
  ATTESTATION_TYPE,
} from "@/app/agents/_registry";

export type RecordObservation = {
  asset: string | null;
  matchesExpectedEnvelope: boolean;
  evidenceSource: "configured_das";
  subjectAsset: string | null;
  authority: string | null;
  responseHash: string | null;
  recordedAt: number | null;
  reasons: string[];
};

export type ValidationRecordLookup = {
  hasMatchingRecord: boolean;
  count: number;
  latest: RecordObservation | null;
  /** The page cap was hit on a full final page, so more records may exist. */
  truncated: boolean;
};

const str = (v: unknown): string | undefined => (typeof v === "string" ? v : undefined);
const field = (d: Record<string, unknown>, snake: string, camel: string): string | undefined =>
  str(d[snake]) ?? str(d[camel]);
const isHex64 = (s: string) => /^[0-9a-f]{64}$/.test(s);
const isPublicKeyLike = (s: string) =>
  s.length >= 32 && s.length <= 44 && /^[1-9A-HJ-NP-Za-km-z]+$/.test(s);
const unsafeAttestationText =
  /[\p{Cc}\u061C\u200B-\u200F\u2028-\u202E\u2060-\u2064\u2066-\u2069\uFEFF]/u;
const isSafeAttestationText = (s: string) =>
  s.length > 0 &&
  new TextEncoder().encode(s).length <= 200 &&
  !unsafeAttestationText.test(s);
const SUBJECT_REGISTRY = "mpl-agent-014";
const MAX_UNIX_SECONDS = 253_402_300_799;

const obj = (v: unknown): Record<string, unknown> | undefined =>
  v && typeof v === "object" ? (v as Record<string, unknown>) : undefined;

// Helius reports the AppData write authority under adapter_config; a flat
// fallback covers other DAS shapes. The plugin's top-level `authority` is the
// adapter config authority and is not the write authority, so it is ignored.
function writeAuthority(plugin: Record<string, unknown>): string | null {
  const cfg = obj(plugin["adapter_config"]) ?? obj(plugin["adapterConfig"]);
  const da =
    obj(cfg?.["data_authority"]) ??
    obj(cfg?.["dataAuthority"]) ??
    obj(plugin["data_authority"]) ??
    obj(plugin["dataAuthority"]);
  return str(da?.["address"]) ?? null;
}

export function appData(
  asset: Record<string, unknown>,
): Record<string, unknown> | null {
  const plugins = asset["external_plugins"];
  if (!Array.isArray(plugins)) return null;
  return plugins.find((plugin) => obj(plugin)?.["type"] === "AppData") ?? null;
}

/** Structural inspection of one DAS-reported validation record. */
export function inspectValidationRecord(
  asset: Record<string, unknown>,
  authority: string,
): RecordObservation {
  const id = str(asset["id"]) ?? null;
  const plugin = appData(asset);
  if (!plugin) {
    return {
      asset: id,
      matchesExpectedEnvelope: false,
      evidenceSource: "configured_das",
      subjectAsset: null,
      authority: null,
      responseHash: null,
      recordedAt: null,
      reasons: ["no AppData external plugin on this asset"],
    };
  }
  const data = obj(plugin["data"]) ?? {};
  const dataAuthority = writeAuthority(plugin);
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

  const subject = obj(data["subject"]);
  if (!subject) reasons.push("subject object missing");
  if (field(subject ?? {}, "registry", "registry") !== SUBJECT_REGISTRY) {
    reasons.push(`subject.registry is not ${SUBJECT_REGISTRY}`);
  }
  const subjectAsset = field(subject ?? {}, "asset", "asset") ?? null;
  if (!subjectAsset) reasons.push("subject.asset missing");
  else if (!isPublicKeyLike(subjectAsset)) {
    reasons.push("subject.asset is not a base58 Solana-address shape");
  }
  const subjectRegistration = subject?.["registration"];
  if (
    subjectRegistration !== undefined &&
    (typeof subjectRegistration !== "string" ||
      !isPublicKeyLike(subjectRegistration))
  ) {
    reasons.push("subject.registration is not a base58 Solana-address shape");
  }
  const subjectAgentId = subject?.["agent_id"] ?? subject?.["agentId"];
  if (
    subjectAgentId !== undefined &&
    (typeof subjectAgentId !== "string" ||
      !isSafeAttestationText(subjectAgentId))
  ) {
    reasons.push("subject.agentId is not a safe non-empty string");
  }

  const tag = field(data, "tag", "tag");
  if (!tag || !isSafeAttestationText(tag))
    reasons.push("tag missing, empty, or unsafe");

  const covenant = obj(data["covenant"]);
  if (!covenant) reasons.push("covenant object missing");
  const releaseTarget = field(
    covenant ?? {},
    "release_target",
    "releaseTarget",
  );
  const releaseSubject = field(
    covenant ?? {},
    "release_subject",
    "releaseSubject",
  );
  const releaseScope = field(covenant ?? {}, "release_scope", "releaseScope");
  if (!releaseTarget || !isSafeAttestationText(releaseTarget)) {
    reasons.push("covenant.releaseTarget missing, empty, or unsafe");
  }
  if (!releaseSubject || !isSafeAttestationText(releaseSubject)) {
    reasons.push("covenant.releaseSubject missing, empty, or unsafe");
  }
  if (!releaseScope || !isSafeAttestationText(releaseScope)) {
    reasons.push("covenant.releaseScope missing, empty, or unsafe");
  }
  if (tag && releaseScope && tag !== releaseScope) {
    reasons.push("tag does not match covenant.releaseScope");
  }

  const recordedRaw = data["recorded_at"] ?? data["recordedAt"];
  const recordedAt =
    Number.isSafeInteger(recordedRaw) &&
    (recordedRaw as number) >= 0 &&
    (recordedRaw as number) <= MAX_UNIX_SECONDS
      ? (recordedRaw as number)
      : null;
  if (recordedRaw == null) {
    reasons.push("recordedAt missing");
  } else if (recordedAt == null) {
    reasons.push("recordedAt is outside the supported Unix-seconds range");
  }

  return {
    asset: id,
    matchesExpectedEnvelope: reasons.length === 0,
    evidenceSource: "configured_das",
    subjectAsset,
    authority: dataAuthority,
    responseHash,
    recordedAt,
    reasons,
  };
}

type Rpc = (method: string, params: unknown) => Promise<unknown>;

/** Find DAS-reported records with the expected envelope and subject. */
export async function findValidationRecords(
  rpc: Rpc,
  agent: string,
  authority: string,
): Promise<ValidationRecordLookup> {
  const MAX_PAGES = 5;
  const matches: RecordObservation[] = [];
  let truncated = false;
  for (let page = 1; page <= MAX_PAGES; page += 1) {
    const value = await rpc("getAssetsByOwner", {
      ownerAddress: authority,
      page,
      limit: 1000,
    });
    const resp = obj(value);
    if (!resp) throw new Error("DAS response is not an object");
    const rawItems = resp["items"];
    if (!Array.isArray(rawItems)) throw new Error("DAS items are not an array");
    const items = rawItems.filter((item): item is Record<string, unknown> =>
      Boolean(obj(item)),
    );
    if (items.length === 0) break;
    for (const item of items) {
      const observation = inspectValidationRecord(item, authority);
      if (
        observation.matchesExpectedEnvelope &&
        observation.subjectAsset === agent
      ) {
        matches.push(observation);
      }
    }
    if (items.length < 1000) break;
    if (page === MAX_PAGES) truncated = true; // full final page at the cap; more may exist
  }
  const latest = matches.reduce<RecordObservation | null>(
    (acc, v) => (acc && (acc.recordedAt ?? 0) >= (v.recordedAt ?? 0) ? acc : v),
    null,
  );
  return {
    hasMatchingRecord: matches.length > 0,
    count: matches.length,
    latest,
    truncated,
  };
}
