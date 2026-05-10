#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const releaseId = "tmp-alpha-readiness-gate-validation";
const acceptedReleaseId = "tmp-alpha-accepted-fixture-validation";
const bundleDir = join(repoRoot, "docs", "releases", releaseId);
const evidencePath = join(bundleDir, "evidence.json");
const notesPath = join(bundleDir, "validation.md");
const manifestPath = join(bundleDir, "manifest.json");
const acceptedBundleDir = join(repoRoot, "docs", "releases", acceptedReleaseId);
const localHomeMarker = ["", "Users", ""].join("/");
const evidenceSchema = "covenant.alpha-release-evidence.v1";
const acceptedCommands = [
  "node agent-os/scripts/alpha-release-readiness.mjs --strict",
  "bash agent-os/scripts/validate.sh --quick",
];

function run(args) {
  const result = spawnSync(process.execPath, args, {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  return {
    status: result.status ?? 1,
    stdout: (result.stdout || "").trim(),
    stderr: (result.stderr || "").trim(),
  };
}

function fail(errors) {
  console.error("validate-alpha-release-evidence: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

function cleanup() {
  if (existsSync(bundleDir)) {
    rmSync(bundleDir, { recursive: true, force: true });
  }
  if (existsSync(acceptedBundleDir)) {
    rmSync(acceptedBundleDir, { recursive: true, force: true });
  }
}

function fileDigest(path) {
  const bytes = readFileSync(path);
  return {
    bytes: bytes.length,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  };
}

function bundleFiles(dir, prefix = "") {
  const files = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((left, right) => left.name.localeCompare(right.name))) {
    const path = prefix ? `${prefix}/${entry.name}` : entry.name;
    const absolutePath = join(dir, entry.name);
    if (path === "manifest.json") {
      continue;
    }
    if (entry.isDirectory()) {
      files.push(...bundleFiles(absolutePath, path));
      continue;
    }
    if (entry.isFile()) {
      files.push(path);
    }
  }
  return files;
}

function writeManifest(dir, id) {
  const manifest = {
    schema: "covenant.alpha-release-manifest.v1",
    kind: "alpha_release_manifest",
    release_id: id,
    files: bundleFiles(dir).map((path) => ({
      path,
      ...fileDigest(join(dir, path)),
    })),
  };
  writeFileSync(join(dir, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
}

const existingBundle = [bundleDir, acceptedBundleDir].find((dir) => existsSync(dir));
if (existingBundle) {
  fail([`${existingBundle.replace(`${repoRoot}/`, "")} already exists; remove it before running this validation`]);
}

const errors = [];

function writeAcceptedFixture() {
  const evidence = {
    schema: evidenceSchema,
    kind: "alpha_release_evidence",
    generated_at: "2026-01-01T00:00:00.000Z",
    commit: "0123456789abcdef0123456789abcdef01234567",
    commit_short: "0123456",
    branch: "main",
    dirty_files: 0,
    readiness: {
      kind: "alpha_release_readiness",
      generated_at: "2026-01-01T00:00:00.000Z",
      ready: true,
      blockers: [],
      checks: [
        {
          id: "clean-working-tree",
          title: "Working tree is clean",
          severity: "blocker",
          ok: true,
          command: "git status --porcelain",
        },
      ],
    },
    commands: acceptedCommands,
    notes: ["Synthetic accepted fixture for bundle validator coverage."],
  };
  const gates = acceptedCommands
    .map((command) => `- [x] \`${command}\` - result: passed`)
    .join("\n");
  const notes = `# ${acceptedReleaseId} Validation Notes

Status: accepted
Generated: 2026-01-01T00:00:00.000Z
Candidate commit: ${evidence.commit}
Branch: main
Dirty files: 0
Alpha readiness: ready

## Required Gates

${gates}

## Alpha Readiness

Blockers:

- none

## Live Prerequisites

- [x] All required live prerequisites recorded.

## Release Notes

- Synthetic accepted fixture for bundle validator coverage.

## Decision

accepted
`;

  mkdirSync(acceptedBundleDir, { recursive: true });
  writeFileSync(join(acceptedBundleDir, "evidence.json"), `${JSON.stringify(evidence, null, 2)}\n`);
  writeFileSync(join(acceptedBundleDir, "validation.md"), notes);
  writeManifest(acceptedBundleDir, acceptedReleaseId);
}

try {
  const evidence = run(["agent-os/scripts/alpha-release-evidence.mjs", "--json"]);
  if (evidence.status !== 0) {
    errors.push(`alpha-release-evidence failed: ${evidence.stderr || evidence.stdout}`);
  } else {
    const data = JSON.parse(evidence.stdout);
    if (data.schema !== evidenceSchema) {
      errors.push("evidence schema mismatch");
    }
    if (data.kind !== "alpha_release_evidence") {
      errors.push("evidence kind mismatch");
    }
    if (data.readiness?.kind !== "alpha_release_readiness") {
      errors.push("evidence readiness kind mismatch");
    }
    if (!Array.isArray(data.readiness?.checks) || data.readiness.checks.length === 0) {
      errors.push("evidence readiness checks missing");
    }
    if (JSON.stringify(data.readiness).includes(localHomeMarker)) {
      errors.push("evidence readiness must not include machine-local paths");
    }
    if (data.readiness?.checks?.some((check) => Object.hasOwn(check, "output"))) {
      errors.push("evidence readiness checks must not include raw command output");
    }
  }

  const bundle = run(["agent-os/scripts/alpha-release-bundle.mjs", releaseId]);
  if (bundle.status !== 0) {
    errors.push(`alpha-release-bundle failed: ${bundle.stderr || bundle.stdout}`);
  }

  const draftValidation = run([
    "agent-os/scripts/alpha-release-validate-bundle.mjs",
    releaseId,
    "--allow-dirty",
    "--allow-pending",
    "--allow-draft",
    "--allow-blocked-readiness",
  ]);
  if (draftValidation.status !== 0) {
    errors.push(`draft bundle validation failed: ${draftValidation.stderr || draftValidation.stdout}`);
  }

  const notes = existsSync(notesPath) ? readFileSync(notesPath, "utf8") : "";
  if (notes) {
    const manifest = existsSync(manifestPath) ? readFileSync(manifestPath, "utf8") : "";
    if (!manifest.includes("covenant.alpha-release-manifest.v1")) {
      errors.push("bundle manifest schema missing");
    }
    writeFileSync(notesPath, `${notes}\n`);
    const staleManifestValidation = run([
      "agent-os/scripts/alpha-release-validate-bundle.mjs",
      releaseId,
      "--allow-dirty",
      "--allow-pending",
      "--allow-draft",
      "--allow-blocked-readiness",
    ]);
    if (staleManifestValidation.status === 0) {
      errors.push("bundle validation accepted stale manifest digests after notes changed");
    }
    writeFileSync(notesPath, notes);
    writeManifest(bundleDir, releaseId);

    const extraPath = join(bundleDir, "unmanifested.txt");
    writeFileSync(extraPath, "not recorded in manifest\n");
    const unmanifestedFileValidation = run([
      "agent-os/scripts/alpha-release-validate-bundle.mjs",
      releaseId,
      "--allow-dirty",
      "--allow-pending",
      "--allow-draft",
      "--allow-blocked-readiness",
    ]);
    if (unmanifestedFileValidation.status === 0) {
      errors.push("bundle validation accepted an unmanifested bundle file");
    }
    rmSync(extraPath, { force: true });
    writeManifest(bundleDir, releaseId);

    writeFileSync(notesPath, notes.replace(" - result: pending", ""));
    writeManifest(bundleDir, releaseId);
    const missingResultValidation = run([
      "agent-os/scripts/alpha-release-validate-bundle.mjs",
      releaseId,
      "--allow-dirty",
      "--allow-pending",
      "--allow-draft",
      "--allow-blocked-readiness",
    ]);
    if (missingResultValidation.status === 0) {
      errors.push("bundle validation accepted a command without a gate result");
    }

    writeFileSync(notesPath, notes.replace("result: pending", "result: skipped"));
    writeManifest(bundleDir, releaseId);
    const skippedWithoutReasonValidation = run([
      "agent-os/scripts/alpha-release-validate-bundle.mjs",
      releaseId,
      "--allow-dirty",
      "--allow-pending",
      "--allow-draft",
      "--allow-blocked-readiness",
    ]);
    if (skippedWithoutReasonValidation.status === 0) {
      errors.push("bundle validation accepted a skipped gate without a reason");
    }
    writeFileSync(notesPath, notes);
    writeManifest(bundleDir, releaseId);

    const generatedEvidence = JSON.parse(readFileSync(evidencePath, "utf8"));
    const outsideGateLines = generatedEvidence.commands
      .map((command) => `- [x] \`${command}\` - result: passed`)
      .join("\n");
    const commandOutsideRequiredGates = notes.replace(
      /## Required Gates\n\n[\s\S]*?\n\n## Alpha Readiness/,
      `## Required Gates\n\n- none\n\n## Alpha Readiness`,
    ).replace(
      "\n## Decision\n",
      `\n## Copied Gate Lines\n\n${outsideGateLines}\n\n## Decision\n`,
    );
    writeFileSync(notesPath, commandOutsideRequiredGates);
    writeManifest(bundleDir, releaseId);
    const outsideSectionValidation = run([
      "agent-os/scripts/alpha-release-validate-bundle.mjs",
      releaseId,
      "--allow-dirty",
      "--allow-blocked-readiness",
      "--allow-draft",
    ]);
    if (outsideSectionValidation.status === 0) {
      errors.push("bundle validation accepted evidence commands outside Required Gates");
    }
    writeFileSync(notesPath, notes);
    writeManifest(bundleDir, releaseId);

    const blockerOutsideAlphaReadiness = notes.replace(
      /## Alpha Readiness\n\nBlockers:\n\n[\s\S]*?\n\n## Live Prerequisites/,
      `## Alpha Readiness\n\nBlockers:\n\n- none\n\n## Live Prerequisites`,
    ).replace(
      "\n## Decision\n",
      `\n## Copied Readiness Blockers\n\n${generatedEvidence.readiness.blockers
        .map((blocker) => `- ${blocker}`)
        .join("\n")}\n\n## Decision\n`,
    );
    writeFileSync(notesPath, blockerOutsideAlphaReadiness);
    writeManifest(bundleDir, releaseId);
    const outsideReadinessValidation = run([
      "agent-os/scripts/alpha-release-validate-bundle.mjs",
      releaseId,
      "--allow-dirty",
      "--allow-pending",
      "--allow-draft",
      "--allow-blocked-readiness",
    ]);
    if (outsideReadinessValidation.status === 0) {
      errors.push("bundle validation accepted readiness blockers outside Alpha Readiness");
    }
    writeFileSync(notesPath, notes);
    writeManifest(bundleDir, releaseId);

    const commitMismatch = notes.replace(
      /^Candidate commit: .+$/m,
      "Candidate commit: ffffffffffffffffffffffffffffffffffffffff",
    );
    writeFileSync(notesPath, commitMismatch);
    writeManifest(bundleDir, releaseId);
    const commitMismatchValidation = run([
      "agent-os/scripts/alpha-release-validate-bundle.mjs",
      releaseId,
      "--allow-dirty",
      "--allow-pending",
      "--allow-draft",
      "--allow-blocked-readiness",
    ]);
    if (commitMismatchValidation.status === 0) {
      errors.push("bundle validation accepted a candidate commit mismatch");
    }
    writeFileSync(notesPath, notes);
    writeManifest(bundleDir, releaseId);

    const readinessMismatch = notes.replace(/^Alpha readiness: .+$/m, "Alpha readiness: ready");
    writeFileSync(notesPath, readinessMismatch);
    writeManifest(bundleDir, releaseId);
    const readinessMismatchValidation = run([
      "agent-os/scripts/alpha-release-validate-bundle.mjs",
      releaseId,
      "--allow-dirty",
      "--allow-pending",
      "--allow-draft",
      "--allow-blocked-readiness",
    ]);
    if (readinessMismatchValidation.status === 0) {
      errors.push("bundle validation accepted an alpha readiness metadata mismatch");
    }
    writeFileSync(notesPath, notes);
    writeManifest(bundleDir, releaseId);

    const acceptedWithFailedGate = notes
      .replaceAll("- [ ]", "- [x]")
      .replaceAll("result: pending", "result: passed")
      .replace("result: passed", "result: failed")
      .replace(/\n## Decision\n\ndraft\n/, "\n## Decision\n\naccepted\n");
    writeFileSync(notesPath, acceptedWithFailedGate);
    writeManifest(bundleDir, releaseId);
    const acceptedFailedValidation = run([
      "agent-os/scripts/alpha-release-validate-bundle.mjs",
      releaseId,
      "--allow-dirty",
      "--allow-blocked-readiness",
    ]);
    if (acceptedFailedValidation.status === 0) {
      errors.push("bundle validation accepted a failed gate with decision accepted");
    }

    const acceptedWithSkippedGate = notes
      .replaceAll("- [ ]", "- [x]")
      .replaceAll("result: pending", "result: passed")
      .replace("result: passed", "result: skipped: unavailable on this host")
      .replace(/\n## Decision\n\ndraft\n/, "\n## Decision\n\naccepted\n");
    writeFileSync(notesPath, acceptedWithSkippedGate);
    writeManifest(bundleDir, releaseId);
    const acceptedSkippedValidation = run([
      "agent-os/scripts/alpha-release-validate-bundle.mjs",
      releaseId,
      "--allow-dirty",
      "--allow-blocked-readiness",
    ]);
    if (acceptedSkippedValidation.status === 0) {
      errors.push("bundle validation accepted a skipped gate with decision accepted");
    }

    writeFileSync(notesPath, notes);
    writeManifest(bundleDir, releaseId);
  }

  const acceptanceValidation = run([
    "agent-os/scripts/alpha-release-validate-bundle.mjs",
    releaseId,
    "--allow-dirty",
    "--allow-pending",
    "--allow-draft",
  ]);
  const parsedEvidence = existsSync(evidencePath)
    ? JSON.parse(readFileSync(evidencePath, "utf8"))
    : null;
  if (parsedEvidence?.readiness?.ready === false && acceptanceValidation.status === 0) {
    errors.push("blocked readiness was accepted without --allow-blocked-readiness");
  }

  writeAcceptedFixture();
  const acceptedFixtureValidation = run([
    "agent-os/scripts/alpha-release-validate-bundle.mjs",
    acceptedReleaseId,
  ]);
  if (acceptedFixtureValidation.status !== 0) {
    errors.push(`accepted bundle fixture failed: ${acceptedFixtureValidation.stderr || acceptedFixtureValidation.stdout}`);
  }
} catch (error) {
  errors.push(error.message);
} finally {
  cleanup();
}

if (errors.length > 0) {
  fail(errors);
}

console.log("validate-alpha-release-evidence: ok");
