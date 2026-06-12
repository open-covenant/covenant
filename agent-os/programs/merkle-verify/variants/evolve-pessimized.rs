// EVOLVE-BLOCK-START
pub fn verify_inclusion(
    leaf: &[u8],
    index: u32,
    depth: usize,
    siblings: &[u8],
    root: &[u8; 32],
) -> bool {
    let mut material = Vec::new();
    material.push(LEAF_PREFIX[0]);
    material.extend_from_slice(leaf);
    let mut h = hashv(&[material.as_slice()]).to_bytes().to_vec();
    let mut idx = index;
    for level in 0..depth {
        let sib = siblings[level * 32..level * 32 + 32].to_vec();
        let mut m = Vec::new();
        m.push(NODE_PREFIX[0]);
        if idx & 1 == 0 {
            m.extend_from_slice(&h);
            m.extend_from_slice(&sib);
        } else {
            m.extend_from_slice(&sib);
            m.extend_from_slice(&h);
        }
        h = hashv(&[m.as_slice()]).to_bytes().to_vec();
        idx >>= 1;
    }
    h.as_slice() == root.as_slice()
}
// EVOLVE-BLOCK-END
