// EVOLVE-BLOCK-START
pub fn verify_inclusion(
    leaf: &[u8],
    index: u32,
    depth: usize,
    siblings: &[u8],
    root: &[u8; 32],
) -> bool {
    // Single contiguous stack buffer per hash -> one memory region for the
    // sol_sha256 syscall to translate (the multi-slice form pays a
    // per-region cost). Buffer reused across levels, zero allocation.
    let mut lbuf = [0u8; 33];
    lbuf[0] = LEAF_PREFIX[0];
    lbuf[1..].copy_from_slice(leaf);
    let mut h = hashv(&[&lbuf]).to_bytes();
    let mut nbuf = [0u8; 65];
    nbuf[0] = NODE_PREFIX[0];
    let mut idx = index;
    for level in 0..depth {
        let sib = &siblings[level * 32..level * 32 + 32];
        if idx & 1 == 0 {
            nbuf[1..33].copy_from_slice(&h);
            nbuf[33..].copy_from_slice(sib);
        } else {
            nbuf[1..33].copy_from_slice(sib);
            nbuf[33..].copy_from_slice(&h);
        }
        h = hashv(&[&nbuf]).to_bytes();
        idx >>= 1;
    }
    &h == root
}
// EVOLVE-BLOCK-END
