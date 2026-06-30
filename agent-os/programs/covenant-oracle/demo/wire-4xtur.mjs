// Convenience entrypoint for the featured 4XtUr mainnet identity. All gating
// logic lives in the canonical wire-gate.mjs; this only pins the asset.
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const ASSET = '4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc';
const script = fileURLToPath(new URL('./wire-gate.mjs', import.meta.url));
process.exit(spawnSync(process.execPath, [script, ASSET], { stdio: 'inherit' }).status ?? 1);
