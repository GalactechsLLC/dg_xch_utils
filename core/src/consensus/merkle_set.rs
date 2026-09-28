//! Merkle-set construction and proof validation for wallet requests.

use crate::utils::hash_256;

/// The 30-byte zero prefix + two node-type bytes that domain-separate a middle-node hash:
/// `Sha256(0u8*30 || encode_type(l) || encode_type(r) || l || r)`.
const HASH_PREFIX: [u8; 30] = [0u8; 30];

/// The empty-set root / empty-subtree hash (all zeros).
pub const BLANK: [u8; 32] = [0u8; 32];

// Internal placeholder for empty nodes retained in the proof tree.
const EMPTY_NODE_HASH: [u8; 32] = [
    0x66, 0x68, 0x7a, 0xad, 0xf8, 0x62, 0xbd, 0x77, 0x6c, 0x8f, 0xc1, 0x8b, 0x8e, 0x9f, 0x8e, 0x20,
    0x08, 0x97, 0x14, 0x85, 0x6e, 0xe2, 0x33, 0xb3, 0x90, 0x2a, 0x59, 0x1d, 0x0d, 0x5f, 0x29, 0x25,
];

// Proof byte codes.
const EMPTY: u8 = 0;
const TERMINAL: u8 = 1;
const MIDDLE: u8 = 2;
const TRUNCATED: u8 = 3;

/// Node classification for hash computation. `MidDbl` is a middle node whose subtree
/// collapses to a terminal pair (both children terminals, or a one-sided chain ending in
/// such a pair); it decides where Empty nodes must be inserted.
#[derive(PartialEq, Eq, Copy, Clone, Debug)]
enum NodeType {
    Empty,
    Term,
    Mid,
    MidDbl,
}

fn encode_type(t: NodeType) -> u8 {
    match t {
        NodeType::Empty => 0,
        NodeType::Term => 1,
        NodeType::Mid | NodeType::MidDbl => 2,
    }
}

// the domain-separated middle-node hash
fn hash(ltype: NodeType, rtype: NodeType, left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(30 + 2 + 32 + 32);
    buf.extend_from_slice(&HASH_PREFIX);
    buf.push(encode_type(ltype));
    buf.push(encode_type(rtype));
    buf.extend_from_slice(left);
    buf.extend_from_slice(right);
    hash_256(buf)
}

// the single-leaf root is Sha256(TERMINAL || leaf)
fn hash_leaf(leaf: &[u8; 32]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(33);
    buf.push(NodeType::Term as u8);
    buf.extend_from_slice(leaf);
    hash_256(buf)
}

fn get_bit(val: &[u8; 32], bit: u8) -> bool {
    (val[(bit / 8) as usize] & (0x80 >> (bit & 7))) != 0
}

/// The shape of a node retained for proof generation.
#[derive(PartialEq, Debug, Copy, Clone)]
enum StoredKind {
    Leaf,
    Middle(u32, u32),
    Empty,
    Truncated,
}

impl From<StoredKind> for NodeType {
    fn from(val: StoredKind) -> NodeType {
        match val {
            StoredKind::Empty => NodeType::Empty,
            StoredKind::Leaf => NodeType::Term,
            StoredKind::Middle(_, _) | StoredKind::Truncated => NodeType::Mid,
        }
    }
}

#[derive(PartialEq, Debug, Copy, Clone)]
struct StoredNode {
    kind: StoredKind,
    hash: [u8; 32],
}

impl StoredNode {
    const fn new(kind: StoredKind, hash: [u8; 32]) -> Self {
        Self { kind, hash }
    }
}

/// A malformed proof (bad node code, truncated bytes, mis-positioned leaf, trailing bytes, or
/// proof-of-a-truncated-subtree).
#[derive(Debug, PartialEq, Eq)]
pub struct SetError;

impl std::fmt::Display for SetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid merkle set / proof")
    }
}

impl std::error::Error for SetError {}

/// A retained merkle set over 32-byte leaves: all nodes in a vec, root last.
#[derive(PartialEq, Debug, Clone, Default)]
pub struct MerkleSet {
    nodes: Vec<StoredNode>,
    from_proof: bool,
}

impl MerkleSet {
    /// Build the set from (already-hashed) 32-byte leaves; order and duplicates do not
    /// affect the tree.
    #[must_use]
    pub fn from_leafs(leafs: &mut [[u8; 32]]) -> MerkleSet {
        let mut merkle_tree = MerkleSet {
            from_proof: false,
            ..Default::default()
        };
        if leafs.is_empty() {
            merkle_tree
                .nodes
                .push(StoredNode::new(StoredKind::Empty, BLANK));
            return merkle_tree;
        }
        merkle_tree.generate_merkle_tree_recurse(leafs, 0);
        merkle_tree
    }

    /// Rebuild a (possibly truncated) tree from proof bytes.
    ///
    /// # Errors
    /// Returns [`SetError`] on any malformed proof (never panics on attacker bytes).
    pub fn from_proof(proof: &[u8]) -> Result<MerkleSet, SetError> {
        let mut merkle_tree = MerkleSet {
            from_proof: true,
            ..Default::default()
        };
        merkle_tree.deserialize_proof_impl(proof)?;
        Ok(merkle_tree)
    }

    // Iterative pre-order parse with a leaf position audit (each TERMINAL's bits must match
    // the branch route that reaches it).
    fn deserialize_proof_impl(&mut self, proof: &[u8]) -> Result<(), SetError> {
        enum ParseOp {
            Node,
            Middle,
        }

        fn read_exact<'a>(
            proof: &'a [u8],
            pos: &mut usize,
            n: usize,
        ) -> Result<&'a [u8], SetError> {
            let end = pos.checked_add(n).ok_or(SetError)?;
            let out = proof.get(*pos..end).ok_or(SetError)?;
            *pos = end;
            Ok(out)
        }
        let mut pos = 0usize;

        let mut values = Vec::<(u32, NodeType)>::new();
        let mut ops = vec![ParseOp::Node];
        let mut depth = 0u32;
        let mut bits_stack: Vec<Vec<bool>> = vec![Vec::new()];

        while let Some(op) = ops.pop() {
            let Some(bits) = bits_stack.pop() else {
                return Err(SetError);
            };
            match op {
                ParseOp::Node => {
                    let b = read_exact(proof, &mut pos, 1)?[0];
                    match b {
                        EMPTY => {
                            values.push((self.nodes.len() as u32, NodeType::Empty));
                            self.nodes.push(StoredNode::new(StoredKind::Empty, BLANK));
                        }
                        TERMINAL => {
                            let mut leaf = [0u8; 32];
                            leaf.copy_from_slice(read_exact(proof, &mut pos, 32)?);
                            // audit the leaf is correctly positioned: its bits must retrace the route
                            for (p, v) in bits.iter().enumerate() {
                                if get_bit(&leaf, p as u8) != *v {
                                    return Err(SetError);
                                }
                            }
                            values.push((self.nodes.len() as u32, NodeType::Term));
                            self.nodes.push(StoredNode::new(StoredKind::Leaf, leaf));
                        }
                        TRUNCATED => {
                            let mut th = [0u8; 32];
                            th.copy_from_slice(read_exact(proof, &mut pos, 32)?);
                            values.push((self.nodes.len() as u32, NodeType::Mid));
                            self.nodes.push(StoredNode::new(StoredKind::Truncated, th));
                        }
                        MIDDLE => {
                            if depth > 256 {
                                return Err(SetError);
                            }
                            ops.push(ParseOp::Middle);
                            ops.push(ParseOp::Node);
                            ops.push(ParseOp::Node);

                            bits_stack.push(Vec::new()); // mid is not audited: placeholder
                            let mut new_bits = bits.clone();
                            new_bits.push(true); // processed second => the right branch
                            bits_stack.push(new_bits);
                            let mut new_bits = bits.clone();
                            new_bits.push(false); // processed first => the left branch
                            bits_stack.push(new_bits);

                            depth += 1;
                        }
                        _ => return Err(SetError),
                    }
                }
                ParseOp::Middle => {
                    let right = values.pop().ok_or(SetError)?;
                    let left = values.pop().ok_or(SetError)?;

                    // Proofs carry every tree layer (no collapsing), but node hashes are computed
                    // as-if collapsed: propagate MidDbl up so the collapse points are known.
                    let new_node_type = match (left.1, right.1) {
                        (NodeType::Term, NodeType::Term)
                        | (NodeType::Empty, NodeType::MidDbl)
                        | (NodeType::MidDbl, NodeType::Empty) => NodeType::MidDbl,
                        (_, _) => NodeType::Mid,
                    };

                    let node_hash = match (left.1, right.1) {
                        // A collapsed layer: copy the double-terminal child's hash upward.
                        (NodeType::Empty, NodeType::MidDbl) => {
                            values.push(right);
                            self.nodes[right.0 as usize].hash
                        }
                        (NodeType::MidDbl, NodeType::Empty) => {
                            values.push(left);
                            self.nodes[left.0 as usize].hash
                        }
                        // Not collapsed: hash the pair.
                        (_, _) => {
                            values.push((self.nodes.len() as u32, new_node_type));
                            hash(
                                self.nodes[left.0 as usize].kind.into(),
                                self.nodes[right.0 as usize].kind.into(),
                                &self.nodes[left.0 as usize].hash,
                                &self.nodes[right.0 as usize].hash,
                            )
                        }
                    };
                    self.nodes.push(StoredNode::new(
                        StoredKind::Middle(left.0, right.0),
                        node_hash,
                    ));
                    depth -= 1;
                }
            }
        }
        if pos == proof.len() {
            Ok(())
        } else {
            Err(SetError)
        }
    }

    /// The set's root hash: empty → all-zeros, single leaf → `Sha256(1 || leaf)`.
    #[must_use]
    pub fn get_root(&self) -> [u8; 32] {
        let Some(last) = self.nodes.last() else {
            return BLANK;
        };
        match last.kind {
            StoredKind::Leaf => hash_leaf(&last.hash),
            StoredKind::Middle(_, _) | StoredKind::Truncated => last.hash,
            StoredKind::Empty => BLANK,
        }
    }

    /// Produce the proof that `leaf` is / is not in the set. `true` = proof-of-INCLUSION,
    /// `false` = proof-of-EXCLUSION; the proof bytes verify against [`MerkleSet::get_root`]
    /// via [`validate_merkle_proof`]. On a tree rebuilt `from_proof` the proof bytes come
    /// back empty (truncated subtrees don't round-trip).
    ///
    /// # Errors
    /// Returns [`SetError`] if the lookup walks into a truncated subtree.
    pub fn generate_proof(&self, leaf: &[u8; 32]) -> Result<(bool, Vec<u8>), SetError> {
        let mut proof = Vec::new();
        let included = self.generate_proof_impl(
            self.nodes.len().checked_sub(1).ok_or(SetError)?,
            leaf,
            &mut proof,
            0,
        )?;
        if self.from_proof {
            Ok((included, vec![]))
        } else {
            Ok((included, proof))
        }
    }

    fn generate_proof_impl(
        &self,
        current_node_index: usize,
        leaf: &[u8; 32],
        proof: &mut Vec<u8>,
        depth: u8,
    ) -> Result<bool, SetError> {
        match self.nodes[current_node_index].kind {
            StoredKind::Empty => {
                proof.push(EMPTY);
                Ok(false)
            }
            StoredKind::Leaf => {
                proof.push(TERMINAL);
                proof.extend_from_slice(&self.nodes[current_node_index].hash);
                Ok(&self.nodes[current_node_index].hash == leaf)
            }
            StoredKind::Middle(left, right) => {
                if matches!(
                    (
                        self.nodes[left as usize].kind,
                        self.nodes[right as usize].kind
                    ),
                    (StoredKind::Leaf, StoredKind::Leaf)
                ) {
                    pad_middles_for_proof_gen(
                        proof,
                        &self.nodes[left as usize].hash,
                        &self.nodes[right as usize].hash,
                        depth,
                    );
                    return Ok(&self.nodes[left as usize].hash == leaf
                        || &self.nodes[right as usize].hash == leaf);
                }

                proof.push(MIDDLE);
                if get_bit(leaf, depth) {
                    // bit 1: truncate the left branch, search the right
                    self.other_included(left as usize, proof);
                    self.generate_proof_impl(right as usize, leaf, proof, depth + 1)
                } else {
                    // bit 0: search the left, truncate the right
                    let r = self.generate_proof_impl(left as usize, leaf, proof, depth + 1)?;
                    self.other_included(right as usize, proof);
                    Ok(r)
                }
            }
            StoredKind::Truncated => Err(SetError),
        }
    }

    // The not-traversed sibling subtree, as needed to recompute the root: Empty stays a code,
    // a leaf is TERMINAL (double-terminal collapse needs the real leaf), else TRUNCATED + hash.
    fn other_included(&self, current_node_index: usize, proof: &mut Vec<u8>) {
        match self.nodes[current_node_index].kind {
            StoredKind::Empty => proof.push(EMPTY),
            StoredKind::Middle(_, _) | StoredKind::Truncated => {
                proof.push(TRUNCATED);
                proof.extend_from_slice(&self.nodes[current_node_index].hash);
            }
            StoredKind::Leaf => {
                proof.push(TERMINAL);
                proof.extend_from_slice(&self.nodes[current_node_index].hash);
            }
        }
    }

    // The radix sort that also retains every node (the proof-capable sibling of
    // block_generator.rs::merkle_set_recurse).
    fn generate_merkle_tree_recurse(
        &mut self,
        range: &mut [[u8; 32]],
        depth: u8,
    ) -> ([u8; 32], NodeType) {
        assert!(!range.is_empty(), "empty range in merkle tree recursion");

        if range.len() == 1 {
            self.nodes.push(StoredNode::new(StoredKind::Leaf, range[0]));
            return (range[0], NodeType::Term);
        }

        // Partition on the bit at `depth`: 0-bits left, 1-bits right.
        let mut left: i32 = 0;
        let mut right = range.len() as i32 - 1;
        while left <= right {
            let left_bit = get_bit(&range[left as usize], depth);
            let right_bit = get_bit(&range[right as usize], depth);
            if left_bit && !right_bit {
                range.swap(left as usize, right as usize);
                left += 1;
                right -= 1;
            } else {
                if !left_bit {
                    left += 1;
                }
                if right_bit {
                    right -= 1;
                }
            }
        }

        let left_empty = left == 0;
        let right_empty = right == range.len() as i32 - 1;

        if left_empty || right_empty {
            if depth == 255 {
                // All 256 bits identical: a duplicate value, collapsed to one leaf (it's a set).
                debug_assert!(range.len() > 1);
                debug_assert!(range[0] == range[1]);
                self.nodes.push(StoredNode::new(StoredKind::Leaf, range[0]));
                (range[0], NodeType::Term)
            } else {
                // One-sided at this level: forward the child, inserting an Empty node only when
                // the child is a (non-collapsing) Mid.
                let (child_hash, child_type) = self.generate_merkle_tree_recurse(range, depth + 1);
                if child_type == NodeType::Mid {
                    self.nodes
                        .push(StoredNode::new(StoredKind::Empty, EMPTY_NODE_HASH));
                    let node_length = self.nodes.len() as u32;
                    if left_empty {
                        let node_hash = hash(NodeType::Empty, child_type, &BLANK, &child_hash);
                        self.nodes.push(StoredNode::new(
                            StoredKind::Middle(node_length - 1, node_length - 2),
                            node_hash,
                        ));
                        (node_hash, NodeType::Mid)
                    } else {
                        let node_hash = hash(child_type, NodeType::Empty, &child_hash, &BLANK);
                        self.nodes.push(StoredNode::new(
                            StoredKind::Middle(node_length - 2, node_length - 1),
                            node_hash,
                        ));
                        (node_hash, NodeType::Mid)
                    }
                } else {
                    (child_hash, child_type)
                }
            }
        } else if depth == 255 {
            // Bottom-of-tree split of the last distinct pair (u8 depth would overflow).
            debug_assert!(range.len() > 1);
            debug_assert!(left < range.len() as i32);
            self.nodes.push(StoredNode::new(StoredKind::Leaf, range[0]));
            self.nodes
                .push(StoredNode::new(StoredKind::Leaf, range[left as usize]));
            let nodes_len = self.nodes.len() as u32;
            let node_hash = hash(
                NodeType::Term,
                NodeType::Term,
                &range[0],
                &range[left as usize],
            );
            self.nodes.push(StoredNode::new(
                StoredKind::Middle(nodes_len - 2, nodes_len - 1),
                node_hash,
            ));
            (node_hash, NodeType::MidDbl)
        } else {
            // A middle node proper: recurse both sides.
            let (left_hash, left_type) =
                self.generate_merkle_tree_recurse(&mut range[..left as usize], depth + 1);
            let left_child_index = self.nodes.len() as u32 - 1;
            let (right_hash, right_type) =
                self.generate_merkle_tree_recurse(&mut range[left as usize..], depth + 1);

            let node_hash = hash(left_type, right_type, &left_hash, &right_hash);
            let node_type = if left_type == NodeType::Term && right_type == NodeType::Term {
                NodeType::MidDbl
            } else {
                NodeType::Mid
            };
            self.nodes.push(StoredNode::new(
                StoredKind::Middle(left_child_index, self.nodes.len() as u32 - 1),
                node_hash,
            ));
            (node_hash, node_type)
        }
    }
}

// Re-introduce the collapsed one-sided levels between `depth` and the first bit where the
// two leaves of a double-terminal subtree diverge, so the proof's path structure exactly
// matches the leaves' bits.
fn pad_middles_for_proof_gen(proof: &mut Vec<u8>, left: &[u8; 32], right: &[u8; 32], depth: u8) {
    let left_bit = get_bit(left, depth);
    let right_bit = get_bit(right, depth);
    proof.push(MIDDLE);
    if left_bit != right_bit {
        proof.push(TERMINAL);
        proof.extend_from_slice(left);
        proof.push(TERMINAL);
        proof.extend_from_slice(right);
    } else if left_bit {
        proof.push(EMPTY);
        pad_middles_for_proof_gen(proof, left, right, depth + 1);
    } else {
        pad_middles_for_proof_gen(proof, left, right, depth + 1);
        proof.push(EMPTY);
    }
}

/// Verify `proof` against `root`: `Ok(true)` proves `item` IS in the set, `Ok(false)` proves
/// it is NOT — what a wallet runs against the foliage root.
///
/// # Errors
/// Returns [`SetError`] if the proof is malformed or does not hash to `root`.
pub fn validate_merkle_proof(
    proof: &[u8],
    item: &[u8; 32],
    root: &[u8; 32],
) -> Result<bool, SetError> {
    let tree = MerkleSet::from_proof(proof)?;
    if tree.get_root() != *root {
        return Err(SetError);
    }
    Ok(tree.generate_proof(item)?.0)
}

#[cfg(test)]
#[path = "../../tests/unit/consensus/merkle_set/tests.rs"]
mod tests;
