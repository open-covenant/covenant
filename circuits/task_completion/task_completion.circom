pragma circom 2.1.5;

include "../node_modules/circomlib/circuits/poseidon.circom";
include "../node_modules/circomlib/circuits/bitify.circom";
include "../node_modules/circomlib/circuits/comparators.circom";
include "components/merkle_verifier.circom";

// POSEIDON-VARIANT
// Spec calls for Poseidon2; circomlib ships the original Poseidon
// (Grassi-Khovratovich-Rechberger-Roy-Schofnegger, 2019).
// Poseidon2 circom templates are available as a separate package; swapping them in should be a
// near drop-in substitution (similar constraint count, different round constants).
// For development we use circomlib Poseidon to lock in the constraint shape; the ceremony must
// pin the final variant before any mainnet verification key is minted.

// Domain-separation tags keep task_hash and result_hash invocations in disjoint images even if
// preimage sizes collide. Values are arbitrary but frozen.
template DomainSeparator() {
    signal output task_tag;
    signal output result_tag;
    task_tag <== 1413567307;      // "TASK"
    result_tag <== 1381190740;    // "RSLT"
}

// Poseidon in circomlib takes up to 16 inputs per call. For wider preimages we sponge in
// fixed 15-wide chunks, carrying the running state as the 16th input. The final partial
// chunk is zero-padded so every Poseidon component has identical arity (required for
// component arrays in circom).
template PoseidonSponge(N) {
    signal input tag;
    signal input in[N];
    signal output out;

    var CHUNK = 15;
    var n_chunks = (N + CHUNK - 1) \ CHUNK;
    var padded = n_chunks * CHUNK;

    signal padded_in[padded];
    for (var i = 0; i < N; i++) {
        padded_in[i] <== in[i];
    }
    for (var i = N; i < padded; i++) {
        padded_in[i] <== 0;
    }

    component h[n_chunks];
    signal state[n_chunks + 1];
    state[0] <== tag;

    for (var c = 0; c < n_chunks; c++) {
        h[c] = Poseidon(CHUNK + 1);
        h[c].inputs[0] <== state[c];
        for (var i = 0; i < CHUNK; i++) {
            h[c].inputs[i + 1] <== padded_in[c * CHUNK + i];
        }
        state[c + 1] <== h[c].out;
    }

    out <== state[n_chunks];
}

template TaskCompletion(N_TASK, N_RESULT, K, LOG_K) {
    signal input task_hash;
    signal input result_hash;
    signal input deadline;
    signal input submitted_at;
    signal input criteria_root;

    // Reputation binding (F-2026-02 fix): these public inputs bind the proof
    // to a specific agent + capability + sample vector + task, preventing any
    // holder of a valid proof from writing arbitrary reputation samples.
    signal input agent_did;
    signal input capability_bit;
    signal input sample_hash;
    signal input task_id;

    signal input task_preimage[N_TASK];
    signal input result_preimage[N_RESULT];
    signal input salt;
    signal input criteria_satisfied[K];
    signal input criteria_path[LOG_K];
    signal input criteria_index[LOG_K];

    // Private reputation sample inputs — hashed to produce sample_hash
    signal input rep_quality;
    signal input rep_timeliness;
    signal input rep_availability;
    signal input rep_cost_efficiency;
    signal input rep_honesty;
    signal input rep_disputed;

    component ds = DomainSeparator();

    // 1. Poseidon(salt, task_preimage) == task_hash, domain-separated
    component th = PoseidonSponge(N_TASK + 1);
    th.tag <== ds.task_tag;
    th.in[0] <== salt;
    for (var i = 0; i < N_TASK; i++) {
        th.in[i + 1] <== task_preimage[i];
    }
    task_hash === th.out;

    // 2. Poseidon(result_preimage) == result_hash, domain-separated
    component rh = PoseidonSponge(N_RESULT);
    rh.tag <== ds.result_tag;
    for (var i = 0; i < N_RESULT; i++) {
        rh.in[i] <== result_preimage[i];
    }
    result_hash === rh.out;

    // 3. Merkle verify: each leaf is Poseidon(bit); path hashes up to criteria_root
    component mv = MerkleVerifier(K, LOG_K);
    mv.root <== criteria_root;
    for (var i = 0; i < K; i++) {
        mv.leaves[i] <== criteria_satisfied[i];
    }
    for (var i = 0; i < LOG_K; i++) {
        mv.path[i] <== criteria_path[i];
        mv.index_bits[i] <== criteria_index[i];
    }

    // 4. AND over all criteria bits. Each bit is booleanity-checked, then multiplied in.
    signal running[K + 1];
    running[0] <== 1;
    for (var i = 0; i < K; i++) {
        criteria_satisfied[i] * (criteria_satisfied[i] - 1) === 0;
        running[i + 1] <== running[i] * criteria_satisfied[i];
    }
    running[K] === 1;

    // 5. submitted_at <= deadline via 64-bit LessEqThan
    component le = LessEqThan(64);
    le.in[0] <== submitted_at;
    le.in[1] <== deadline;
    le.out === 1;

    // Range-bind timestamps to 64 bits to prevent field wraparound sneaking around the comparator.
    component sat_bits = Num2Bits(64);
    sat_bits.in <== submitted_at;
    component dl_bits = Num2Bits(64);
    dl_bits.in <== deadline;

    // 6. Reputation binding (F-2026-02): hash private sample vector and constrain
    //    against the public sample_hash. This ensures the proof commits to a specific
    //    reputation sample — the on-chain handler reads (agent_did, capability_bit,
    //    sample_hash, task_id) from public_inputs, not from caller args.
    component sh = Poseidon(6);
    sh.inputs[0] <== rep_quality;
    sh.inputs[1] <== rep_timeliness;
    sh.inputs[2] <== rep_availability;
    sh.inputs[3] <== rep_cost_efficiency;
    sh.inputs[4] <== rep_honesty;
    sh.inputs[5] <== rep_disputed;
    sample_hash === sh.out;

    // Range-bind capability_bit to 7 bits (0..127)
    component cb_bits = Num2Bits(7);
    cb_bits.in <== capability_bit;

    // Booleanity check on rep_disputed
    rep_disputed * (rep_disputed - 1) === 0;

    // Range-bind reputation axes to 16 bits (0..65535)
    component q_bits = Num2Bits(16);
    q_bits.in <== rep_quality;
    component t_bits = Num2Bits(16);
    t_bits.in <== rep_timeliness;
    component a_bits = Num2Bits(16);
    a_bits.in <== rep_availability;
    component ce_bits = Num2Bits(16);
    ce_bits.in <== rep_cost_efficiency;
    component h_bits = Num2Bits(16);
    h_bits.in <== rep_honesty;
}

component main { public [task_hash, result_hash, deadline, submitted_at, criteria_root, agent_did, capability_bit, sample_hash, task_id] } =
    TaskCompletion(16, 32, 8, 3);
