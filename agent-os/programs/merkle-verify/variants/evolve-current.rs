// EVOLVE-BLOCK-START
pub fn verify_inclusion(
    leaf: &[u8],
    index: u32,
    depth: usize,
    siblings: &[u8],
    root: &[u8; 32],
) -> bool {
    let mut h = hashv(&[&LEAF_PREFIX, leaf]).to_bytes();
    let mut idx = index;
    for level in 0..depth {
        let sib = &siblings[level * 32..level * 32 + 32];
        h = if idx & 1 == 0 {
            hashv(&[&NODE_PREFIX, &h, sib]).to_bytes()
        } else {
            hashv(&[&NODE_PREFIX, sib, &h]).to_bytes()
        };
        idx >>= 1;
    }
    &h == root
}
// EVOLVE-BLOCK-END
