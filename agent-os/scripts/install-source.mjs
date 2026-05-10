#!/usr/bin/env node
import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const repoRoot = resolve(root, "..");

function usage() {
  console.log(`usage: scripts/install-source.mjs --prefix path [--profile release|debug] [--dry-run] [--json] [--skip-build] [--artifact-dir path]

Build and install the Covenant daemon and CLI from source into a local prefix.
The installer writes only under the selected prefix:
  <prefix>/bin/covenantd
  <prefix>/bin/covenant
  <prefix>/share/covenant/install-manifest.json`);
}

let prefixInput = "";
let profile = "release";
let dryRun = false;
let json = false;
let skipBuild = false;
let artifactDirInput = "";

const args = process.argv.slice(2);
for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === "--prefix") {
    prefixInput = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--profile") {
    profile = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--dry-run") {
    dryRun = true;
    continue;
  }
  if (arg === "--json") {
    json = true;
    continue;
  }
  if (arg === "--skip-build") {
    skipBuild = true;
    continue;
  }
  if (arg === "--artifact-dir") {
    artifactDirInput = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--help" || arg === "-h") {
    usage();
    process.exit(0);
  }
  usage();
  process.exit(2);
}

if (!prefixInput) {
  console.error("install-source: --prefix is required");
  process.exit(2);
}

if (!["debug", "release"].includes(profile)) {
  console.error("install-source: --profile must be debug or release");
  process.exit(2);
}

if (artifactDirInput && !skipBuild) {
  console.error("install-source: --artifact-dir requires --skip-build");
  process.exit(2);
}

const prefix = resolve(prefixInput);
const targetProfile = profile === "debug" ? "debug" : "release";
const targetDir = artifactDirInput ? resolve(root, artifactDirInput) : join(root, "target", targetProfile);
const manifestRel = "share/covenant/install-manifest.json";
const binaries = [
  {
    name: "covenantd",
    crate: "covenantd",
    source: join(targetDir, "covenantd"),
    installedRel: "bin/covenantd",
  },
  {
    name: "covenant",
    crate: "covenant",
    source: join(targetDir, "covenant"),
    installedRel: "bin/covenant",
  },
];

const buildArgs = [
  "build",
  "-p",
  "covenantd",
  "-p",
  "covenant",
  "--locked",
  ...(profile === "release" ? ["--release"] : []),
];

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function backupId() {
  return `${new Date().toISOString().replace(/[-:.]/g, "")}-${process.pid}`;
}

function ensureInsidePrefix(path) {
  const rel = relative(prefix, path);
  if (rel === "" || rel.startsWith("..") || isAbsolute(rel)) {
    throw new Error(`refusing to write outside install prefix: ${rel || "."}`);
  }
}

function ensureInsideRoot(path) {
  const rel = relative(root, path);
  if (rel === "" || rel.startsWith("..") || isAbsolute(rel)) {
    throw new Error(`refusing to read artifacts outside agent-os: ${rel || "."}`);
  }
}

function sourceCommit() {
  const result = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  if (result.status !== 0) return "unknown";
  return result.stdout.trim() || "unknown";
}

function installPlan() {
  return {
    kind: "covenant_source_install_plan",
    schema: "covenant.source-install.v1",
    dry_run: dryRun,
    prefix,
    profile,
    build: {
      command: ["cargo", ...buildArgs],
      cwd: "agent-os",
      skipped: skipBuild,
      artifact_dir: relative(root, targetDir),
    },
    writes: [
      ...binaries.map((binary) => ({
        type: "binary",
        name: binary.name,
        source: relative(root, binary.source),
        destination: binary.installedRel,
      })),
      {
        type: "manifest",
        schema: "covenant.source-install.v1",
        destination: manifestRel,
      },
    ],
  };
}

function plannedWrites() {
  return [
    ...binaries.map((binary) => ({
      type: "binary",
      path: binary.installedRel,
    })),
    {
      type: "manifest",
      path: manifestRel,
    },
  ];
}

function createRollbackCheckpoint() {
  const existing = plannedWrites()
    .map((write) => ({
      ...write,
      absolute: join(prefix, write.path),
    }))
    .filter((write) => existsSync(write.absolute));

  if (existing.length === 0) return null;

  const id = backupId();
  const backupDir = join("share", "covenant", "backups", id);
  const files = [];

  for (const file of existing) {
    const stat = statSync(file.absolute);
    if (!stat.isFile()) {
      throw new Error(`refusing to replace non-file destination: ${file.path}`);
    }

    const backupPath = join(backupDir, file.path);
    const backupAbsolute = join(prefix, backupPath);
    ensureInsidePrefix(backupAbsolute);
    mkdirSync(dirname(backupAbsolute), { recursive: true });
    copyFileSync(file.absolute, backupAbsolute);
    chmodSync(backupAbsolute, stat.mode & 0o777);

    files.push({
      type: file.type,
      path: file.path,
      backup_path: backupPath,
      bytes: stat.size,
      sha256: sha256(file.absolute),
      mode: (stat.mode & 0o777).toString(8).padStart(4, "0"),
    });
  }

  return {
    schema: "covenant.source-install.rollback-checkpoint.v1",
    id,
    created_at: new Date().toISOString(),
    backup_dir: backupDir,
    files,
  };
}

function installedManifest(rollbackCheckpoint) {
  const manifest = {
    schema: "covenant.source-install.v1",
    generated_at: new Date().toISOString(),
    source_commit: sourceCommit(),
    profile,
    binaries: binaries.map((binary) => {
      const dest = join(prefix, binary.installedRel);
      return {
        name: binary.name,
        crate: binary.crate,
        source_target: relative(root, binary.source),
        installed_path: binary.installedRel,
        bytes: statSync(dest).size,
        sha256: sha256(dest),
      };
    }),
  };
  if (rollbackCheckpoint) {
    manifest.rollback_checkpoint = rollbackCheckpoint;
  }
  return manifest;
}

try {
  for (const binary of binaries) {
    ensureInsidePrefix(join(prefix, binary.installedRel));
  }
  ensureInsidePrefix(join(prefix, manifestRel));
  ensureInsideRoot(targetDir);

  if (dryRun) {
    const output = installPlan();
    if (json) {
      console.log(JSON.stringify(output, null, 2));
    } else {
      console.log(`install-source: dry run for ${prefix}`);
      for (const write of output.writes) {
        console.log(`- ${write.type}: ${write.destination}`);
      }
    }
    process.exit(0);
  }

  if (!skipBuild) {
    const build = spawnSync("cargo", buildArgs, {
      cwd: root,
      stdio: "inherit",
    });
    if (build.status !== 0) {
      process.exit(build.status ?? 1);
    }
  }

  mkdirSync(join(prefix, "bin"), { recursive: true });
  mkdirSync(join(prefix, "share", "covenant"), { recursive: true });
  const rollbackCheckpoint = createRollbackCheckpoint();

  for (const binary of binaries) {
    if (!existsSync(binary.source)) {
      throw new Error(`missing built binary: ${relative(root, binary.source)}`);
    }
    const dest = join(prefix, binary.installedRel);
    copyFileSync(binary.source, dest);
    chmodSync(dest, 0o755);
  }

  const manifest = installedManifest(rollbackCheckpoint);
  writeFileSync(join(prefix, manifestRel), `${JSON.stringify(manifest, null, 2)}\n`);

  if (json) {
    console.log(JSON.stringify({ ...installPlan(), dry_run: false, manifest }, null, 2));
  } else {
    console.log(`install-source: installed Covenant to ${prefix}`);
    console.log(`manifest: ${join(prefix, manifestRel)}`);
  }
} catch (error) {
  console.error(`install-source: ${error.message}`);
  process.exit(1);
}
