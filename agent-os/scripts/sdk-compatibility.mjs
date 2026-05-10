#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function usage() {
  console.log(`usage: sdk-compatibility [--json]

Report workspace SDK compatibility status without publishing packages, creating
tags, or claiming public API stability.`);
}

const args = new Set(process.argv.slice(2));
if (args.has("--help") || args.has("-h")) {
  usage();
  process.exit(0);
}

const asJson = args.has("--json");
for (const arg of args) {
  if (arg !== "--json") {
    usage();
    process.exit(2);
  }
}

const packageDirs = [
  {
    id: "sdk",
    path: "packages/sdk",
    public_name: "@covenant/sdk",
    expected_private: false,
    stability: "workspace_alpha",
  },
  {
    id: "sdk-ui",
    path: "packages/sdk-ui",
    public_name: "@covenant/sdk-ui",
    expected_private: true,
    stability: "workspace_alpha_private",
  },
];

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function sourceExports(packagePath) {
  const indexPath = join(repoRoot, packagePath, "src", "index.ts");
  if (!existsSync(indexPath)) return [];
  const text = readFileSync(indexPath, "utf8");
  return [...text.matchAll(/export\s+(?:type\s+)?(?:\*|\{[\s\S]*?\})\s+from\s+['"]([^'"]+)['"]/g)]
    .map((match) => match[1])
    .sort();
}

function exportFixture(definition, exports) {
  const path = join(definition.path, "compatibility", "exports.v1.json");
  const absolute = join(repoRoot, path);
  if (!existsSync(absolute)) {
    return {
      path,
      present: false,
      matches: false,
      errors: ["export compatibility fixture is missing"],
    };
  }

  const fixture = readJson(absolute);
  const errors = [];
  if (fixture.schema !== "covenant.sdk-exports.v1") {
    errors.push("fixture schema must be covenant.sdk-exports.v1");
  }
  if (fixture.package !== definition.public_name) {
    errors.push(`fixture package must be ${definition.public_name}`);
  }
  if (fixture.stability !== definition.stability) {
    errors.push(`fixture stability must be ${definition.stability}`);
  }
  if (JSON.stringify(fixture.source_exports ?? []) !== JSON.stringify(exports)) {
    errors.push("fixture source_exports do not match src/index.ts");
  }

  return {
    path,
    present: true,
    matches: errors.length === 0,
    schema: fixture.schema ?? null,
    errors,
  };
}

function packageReport(definition) {
  const packageJsonPath = join(repoRoot, definition.path, "package.json");
  const pkg = readJson(packageJsonPath);
  const rootExport = pkg.exports?.["."] ?? {};
  const errors = [];

  if (pkg.name !== definition.public_name) {
    errors.push(`package name must be ${definition.public_name}`);
  }
  if (Boolean(pkg.private) !== definition.expected_private) {
    errors.push(`private flag must be ${definition.expected_private}`);
  }
  if (!pkg.version || typeof pkg.version !== "string") {
    errors.push("version must be a string");
  }
  if (!pkg.main || !pkg.types) {
    errors.push("main and types entries are required");
  }
  if (rootExport.default !== pkg.main || rootExport.types !== pkg.types) {
    errors.push("root export map must match main/types");
  }
  if (!Array.isArray(pkg.files) || !pkg.files.includes("dist")) {
    errors.push("package files must include dist only as the build artifact boundary");
  }
  if (!pkg.scripts?.build || !pkg.scripts?.typecheck) {
    errors.push("build and typecheck scripts are required");
  }
  const exports = sourceExports(definition.path);
  const fixture = exportFixture(definition, exports);
  errors.push(...fixture.errors);

  return {
    id: definition.id,
    path: definition.path,
    name: pkg.name,
    version: pkg.version,
    private: Boolean(pkg.private),
    stability: definition.stability,
    publish_status: pkg.private ? "private_workspace" : "workspace_only",
    export_map: {
      root: rootExport,
      main: pkg.main ?? null,
      types: pkg.types ?? null,
      files: pkg.files ?? [],
    },
    source_exports: exports,
    fixture,
    ok: errors.length === 0,
    errors,
  };
}

const packages = packageDirs.map(packageReport);
const metadataOk = packages.every((pkg) => pkg.ok);

const report = {
  kind: "covenant_sdk_compatibility",
  schema: "covenant.sdk-compatibility.v1",
  generated_at: new Date().toISOString(),
  ready_for_workspace_alpha: metadataOk,
  ready_for_public_sdk: false,
  packages,
  policy: {
    workspace_alpha: [
      "root export maps must stay aligned with main/types metadata",
      "source exports must match committed compatibility fixtures before removal or rename",
      "README examples must not imply npm publication before release approval",
    ],
    public_sdk_blockers: [
      "npm publication is not approved",
      "generated protocol bindings are not covered by compatibility fixtures",
      "public semver support window is not approved",
    ],
  },
  blockers: metadataOk
    ? [
        "npm publication is not approved",
        "generated protocol bindings are not covered by compatibility fixtures",
        "public semver support window is not approved",
      ]
    : ["SDK package metadata validation failed"],
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`sdk compatibility: ${report.ready_for_workspace_alpha ? "workspace alpha ready" : "blocked"}`);
  console.log("public sdk: blocked");
  for (const pkg of packages) {
    console.log(`- ${pkg.ok ? "ok" : "blocked"}: ${pkg.name}`);
    for (const error of pkg.errors) {
      console.log(`  blocker: ${error}`);
    }
  }
}
