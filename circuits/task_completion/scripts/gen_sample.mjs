import { buildPoseidon } from "circomlibjs";
import { writeFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));

const poseidon = await buildPoseidon();
const F = poseidon.F;

const TASK_TAG = 1413567307n;
const RESULT_TAG = 1381190740n;

const task_preimage = Array.from({ length: 16 }, (_, i) => BigInt(i + 1));
const result_preimage = Array.from({ length: 32 }, (_, i) => BigInt(i + 100));
const salt = BigInt(42);

function poseidonSponge(tag, inputs, chunkSize = 15) {
  const nChunks = Math.ceil(inputs.length / chunkSize);
  const padded = Array.from(inputs);
  while (padded.length < nChunks * chunkSize) padded.push(BigInt(0));

  let state = tag;
  for (let c = 0; c < nChunks; c++) {
    const chunk = padded.slice(c * chunkSize, (c + 1) * chunkSize);
    state = F.toObject(poseidon([state, ...chunk]));
  }
  return state;
}

const task_hash = poseidonSponge(TASK_TAG, [salt, ...task_preimage]);
const result_hash = poseidonSponge(RESULT_TAG, result_preimage);

const criteria_satisfied = [1n, 1n, 1n, 1n, 1n, 1n, 1n, 1n];

const hashed_leaves = criteria_satisfied.map((l) =>
  F.toObject(poseidon([l]))
);

// Build Merkle tree bottom-up to compute root
// depth=3, 8 leaves
let level = [...hashed_leaves];
for (let d = 0; d < 3; d++) {
  const next = [];
  for (let i = 0; i < level.length; i += 2) {
    next.push(F.toObject(poseidon([level[i], level[i + 1]])));
  }
  level = next;
}
const criteria_root = level[0];

// Path for leaf index 0 (index_bits = [0,0,0])
// At each level, sibling is the right child
// Level 0: sibling = hashed_leaves[1]
// Level 1: sibling = hash(hashed_leaves[2], hashed_leaves[3])
// Level 2: sibling = hash(hash(hashed_leaves[4],hashed_leaves[5]), hash(hashed_leaves[6],hashed_leaves[7]))

const pair01 = hashed_leaves; // already computed
const l1_0 = F.toObject(poseidon([hashed_leaves[0], hashed_leaves[1]]));
const l1_1 = F.toObject(poseidon([hashed_leaves[2], hashed_leaves[3]]));
const l1_2 = F.toObject(poseidon([hashed_leaves[4], hashed_leaves[5]]));
const l1_3 = F.toObject(poseidon([hashed_leaves[6], hashed_leaves[7]]));
const l2_0 = F.toObject(poseidon([l1_0, l1_1]));
const l2_1 = F.toObject(poseidon([l1_2, l1_3]));

const criteria_path = [
  hashed_leaves[1].toString(),
  l1_1.toString(),
  l2_1.toString(),
];
const criteria_index = ["0", "0", "0"];

const input = {
  task_hash: task_hash.toString(),
  result_hash: result_hash.toString(),
  deadline: "1800000000",
  submitted_at: "1799000000",
  criteria_root: criteria_root.toString(),
  task_preimage: task_preimage.map((x) => x.toString()),
  result_preimage: result_preimage.map((x) => x.toString()),
  salt: salt.toString(),
  criteria_satisfied: criteria_satisfied.map((x) => x.toString()),
  criteria_path,
  criteria_index,
};

const outPath = join(__dirname, "..", "inputs", "sample_input.json");
writeFileSync(outPath, JSON.stringify(input, null, 2) + "\n");
console.log("wrote", outPath);
console.log("task_hash:", task_hash.toString());
console.log("result_hash:", result_hash.toString());
console.log("criteria_root:", criteria_root.toString());
