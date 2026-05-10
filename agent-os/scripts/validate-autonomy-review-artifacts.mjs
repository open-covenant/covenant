#!/usr/bin/env node
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { spawnSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const repoRoot = resolve(root, "..");
const tasksDir = join(root, "autonomy", "tasks");

function usage() {
  console.log(`usage: validate-autonomy-review-artifacts [task-id]

Run the read-only autonomy review artifact toolchain validation.

Checks artifact generation, unsigned verification, signed fixture verification,
and expected tamper rejection without writing files or Git metadata.`);
}

function run(args, input = null) {
  const result = spawnSync(process.execPath, args, {
    cwd: repoRoot,
    input,
    encoding: "utf8",
    stdio: ["pipe", "pipe", "pipe"],
  });
  return {
    status: result.status ?? 1,
    stdout: (result.stdout || "").trimEnd(),
    stderr: (result.stderr || "").trimEnd(),
  };
}

function fail(errors) {
  console.error("validate-autonomy-review-artifacts: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

function parseJson(label, text, errors) {
  try {
    return JSON.parse(text);
  } catch (error) {
    errors.push(`${label}: invalid JSON: ${error.message}`);
    return null;
  }
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function signingPayload(artifact) {
  return Buffer.from(JSON.stringify({
    ...artifact,
    signing: {
      ...artifact.signing,
      signature: "",
    },
  }));
}

function findDefaultTask() {
  const tasks = readdirSync(tasksDir)
    .filter((file) => file.endsWith(".json"))
    .map((file) => JSON.parse(readFileSync(join(tasksDir, file), "utf8")))
    .filter((task) => task.state === "integrated")
    .sort((left, right) => left.id.localeCompare(right.id));

  return tasks.find((task) => task.id === "autonomy-review-artifact-verifier")
    ?? tasks.find((task) => task.id === "autonomy-review-artifact-scaffold")
    ?? tasks[0]
    ?? null;
}

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  usage();
  process.exit(0);
}
if (args.length > 1) {
  usage();
  process.exit(2);
}

const taskId = args[0] ?? findDefaultTask()?.id ?? "";
if (!/^[a-z0-9][a-z0-9-]*$/.test(taskId)) {
  fail(["task id must be lowercase kebab-case or at least one integrated task must exist"]);
}

const errors = [];

const artifactResult = run(["agent-os/scripts/autonomy-review-artifact.mjs", taskId, "--json"]);
if (artifactResult.status !== 0) {
  errors.push(`autonomy-review-artifact failed: ${artifactResult.stderr || artifactResult.stdout}`);
}
const artifact = parseJson("review artifact", artifactResult.stdout, errors);
if (artifact?.kind !== "autonomy_review_artifact") {
  errors.push("review artifact kind mismatch");
}
if (artifact?.task?.id !== taskId) {
  errors.push("review artifact task id mismatch");
}
if (artifact?.signing?.status !== "unsigned") {
  errors.push("review artifact generator must emit unsigned artifacts by default");
}

const verifyResult = run(
  ["agent-os/scripts/autonomy-verify-review-artifact.mjs", "--stdin", "--json"],
  artifactResult.stdout,
);
if (verifyResult.status !== 0) {
  errors.push(`review artifact verifier rejected generated artifact: ${verifyResult.stderr || verifyResult.stdout}`);
}
const verification = parseJson("review artifact verification", verifyResult.stdout, errors);
if (verification?.valid !== true) {
  errors.push("review artifact verification should be valid");
}

if (artifact) {
  const tampered = {
    ...artifact,
    digests: {
      ...artifact.digests,
      task_sha256: "0".repeat(64),
    },
  };
  const tamperedResult = run(
    ["agent-os/scripts/autonomy-verify-review-artifact.mjs", "--stdin", "--json"],
    JSON.stringify(tampered),
  );
  if (tamperedResult.status === 0) {
    errors.push("review artifact verifier accepted a tampered task digest");
  } else {
    const tamperedReport = parseJson("tamper verification", tamperedResult.stdout, errors);
    const tamperDetected = tamperedReport?.errors?.some((error) =>
      error.includes("task_sha256"),
    );
    if (!tamperDetected) {
      errors.push("review artifact verifier did not report task_sha256 tampering");
    }
  }

  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const publicKeySpki = publicKey.export({ type: "spki", format: "der" });
  const publicKeySpkiBase64 = publicKeySpki.toString("base64");
  const signed = {
    ...artifact,
    signing: {
      status: "signed",
      schema: "covenant.autonomy-review-signature.v1",
      algorithm: "ed25519",
      key_id: "fixture-review-key",
      public_key_spki_sha256: sha256(publicKeySpki),
      signed_at: "2026-01-01T00:00:00.000Z",
      custody: {
        policy: "docs/provenance/review-artifact-signing.md",
        public_key_source: "ephemeral-validator-fixture",
        human_approval_required: true,
      },
      signature: "",
    },
  };
  signed.signing.signature = sign(null, signingPayload(signed), privateKey).toString("base64");

  const missingKeyResult = run(
    ["agent-os/scripts/autonomy-verify-review-artifact.mjs", "--stdin", "--json"],
    JSON.stringify(signed),
  );
  if (missingKeyResult.status === 0) {
    errors.push("signed review artifact verifier accepted a signed artifact without a trusted public key");
  } else {
    const missingKeyReport = parseJson("missing-key signed verification", missingKeyResult.stdout, errors);
    const missingKeyDetected = missingKeyReport?.errors?.some((error) =>
      error.includes("trusted-public-key-spki-base64"),
    );
    if (!missingKeyDetected) {
      errors.push("signed review artifact verifier did not require a trusted public key");
    }
  }

  const signedResult = run(
    [
      "agent-os/scripts/autonomy-verify-review-artifact.mjs",
      "--stdin",
      "--json",
      "--trusted-public-key-spki-base64",
      publicKeySpkiBase64,
    ],
    JSON.stringify(signed),
  );
  if (signedResult.status !== 0) {
    errors.push(`signed review artifact verifier rejected fixture: ${signedResult.stderr || signedResult.stdout}`);
  }
  const signedReport = parseJson("signed verification", signedResult.stdout, errors);
  if (signedReport?.valid !== true) {
    errors.push("signed review artifact verification should be valid");
  }

  const tamperedSigned = {
    ...signed,
    task: {
      ...signed.task,
      title: `${signed.task.title} tampered`,
    },
  };
  const tamperedSignedResult = run(
    [
      "agent-os/scripts/autonomy-verify-review-artifact.mjs",
      "--stdin",
      "--json",
      "--trusted-public-key-spki-base64",
      publicKeySpkiBase64,
    ],
    JSON.stringify(tamperedSigned),
  );
  if (tamperedSignedResult.status === 0) {
    errors.push("signed review artifact verifier accepted a tampered signed artifact");
  } else {
    const tamperedSignedReport = parseJson("tampered signed verification", tamperedSignedResult.stdout, errors);
    const signatureTamperDetected = tamperedSignedReport?.errors?.some((error) =>
      error.includes("signature") || error.includes("metadata"),
    );
    if (!signatureTamperDetected) {
      errors.push("signed review artifact verifier did not report signed artifact tampering");
    }
  }
}

if (errors.length > 0) {
  fail(errors);
}

console.log(`validate-autonomy-review-artifacts: ok (${taskId})`);
