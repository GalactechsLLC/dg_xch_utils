//! CLVM serializer that replaces repeated subtrees with back-references.
//!
//! [`sexp_to_bytes`]: crate::clvm::parser::sexp_to_bytes

use crate::blockchain::sized_bytes::Bytes32;
use crate::clvm::program::SerializedProgram;
use crate::clvm::sexp::SExp;
use dg_xch_serialize::{CONS_BOX_MARKER, MAX_SINGLE_BYTE, encode_size};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Cursor, Write};

const BACK_REFERENCE: u8 = 0xfe;

/// Node address: identity key for a live `SExp` node reached through the tree being serialized.
/// Stable for the whole serialization (the tree outlives every memo). Same soundness argument as
/// [`crate::clvm::tree_hash_cache`]: the memo never outlives the tree, so equal addresses imply the
/// same live node. Used ONLY to memoize tree-hash / serialized-length; the dedup itself keys on the
/// hash VALUE, so distinct-address identical-content subtrees still compress.
fn addr(node: &SExp) -> usize {
    std::ptr::from_ref(node) as usize
}

fn atom_tree_hash(atom: &[u8]) -> Bytes32 {
    let mut hasher = Sha256::new();
    hasher.update([1u8]);
    hasher.update(atom);
    let out: [u8; 32] = hasher.finalize().into();
    out.into()
}

fn pair_tree_hash(first: &Bytes32, rest: &Bytes32) -> Bytes32 {
    let mut hasher = Sha256::new();
    hasher.update([2u8]);
    hasher.update(first);
    hasher.update(rest);
    let out: [u8; 32] = hasher.finalize().into();
    out.into()
}

/// Serialized length of an atom without back-references.
fn serialized_length_atom(buf: &[u8]) -> u64 {
    let lb = buf.len() as u64;
    if lb == 0 || (lb == 1 && buf[0] < 128) {
        1
    } else if lb < 0x40 {
        1 + lb
    } else if lb < 0x2000 {
        2 + lb
    } else if lb < 0x0010_0000 {
        3 + lb
    } else if lb < 0x0800_0000 {
        4 + lb
    } else {
        5 + lb
    }
}

/// Encoded atom length for a value with `num_bits` significant bits.
fn atom_length_bits(num_bits: u64) -> Option<u64> {
    if num_bits < 8 {
        return Some(1);
    }
    let num_bytes = num_bits.div_ceil(8);
    if num_bytes < 0x40 {
        Some(1 + num_bytes)
    } else if num_bytes < 0x2000 {
        Some(2 + num_bytes)
    } else if num_bytes < 0x0010_0000 {
        Some(3 + num_bytes)
    } else if num_bytes < 0x0800_0000 {
        Some(4 + num_bytes)
    } else if num_bytes < 0x0004_0000_0000 {
        Some(5 + num_bytes)
    } else {
        None
    }
}

/// Write a single atom in canonical CLVM form.
fn write_atom<W: Write>(f: &mut W, data: &[u8]) -> io::Result<()> {
    if data.is_empty() {
        f.write_all(&[0x80])?;
    } else if data.len() == 1 && data[0] <= MAX_SINGLE_BYTE {
        f.write_all(&[data[0]])?;
    } else {
        encode_size(f, data.len() as u64)?;
        f.write_all(data)?;
    }
    Ok(())
}

fn encode_backref_path(path: &[bool]) -> Vec<u8> {
    let byte_count = (path.len() + 1 + 7) >> 3;
    let mut v = vec![0u8; byte_count];
    let mut index = byte_count - 1;
    let mut mask: u8 = 1;
    for &p in path.iter().rev() {
        if p {
            v[index] |= mask;
        }
        if mask == 0x80 {
            index -= 1;
            mask = 1;
        } else {
            mask <<= 1;
        }
    }
    v[index] |= mask;
    v
}

/// Index of objects currently reachable from the decoder's value stack.
struct BackrefStackIndex {
    root: Bytes32,
    stack: Vec<(Bytes32, Bytes32)>,
    active: HashMap<Bytes32, u32>,
    parents: HashMap<Bytes32, Vec<(Bytes32, bool)>>,
}

impl BackrefStackIndex {
    fn new() -> Self {
        let root = atom_tree_hash(&[]);
        let mut active = HashMap::new();
        active.insert(root, 1);
        Self {
            root,
            stack: Vec::new(),
            active,
            parents: HashMap::new(),
        }
    }

    fn activate(&mut self, hash: Bytes32) {
        let previous_root = self.root;
        let new_root = pair_tree_hash(&hash, &previous_root);
        self.stack.push((hash, previous_root));
        *self.active.entry(hash).or_default() += 1;
        *self.active.entry(new_root).or_default() += 1;
        self.parents
            .entry(hash)
            .or_default()
            .push((new_root, false));
        self.parents
            .entry(previous_root)
            .or_default()
            .push((new_root, true));
        self.root = new_root;
    }

    fn deactivate_top(&mut self) -> (Bytes32, Bytes32) {
        let item = self.stack.pop().expect("read stack empty");
        if let Some(count) = self.active.get_mut(&item.0) {
            *count = count.saturating_sub(1);
        }
        if let Some(count) = self.active.get_mut(&self.root) {
            *count = count.saturating_sub(1);
        }
        self.root = item.1;
        item
    }

    fn combine_top_pair(&mut self) {
        let right = self.deactivate_top();
        let left = self.deactivate_top();
        *self.active.entry(left.0).or_default() += 1;
        *self.active.entry(right.0).or_default() += 1;
        let pair = pair_tree_hash(&left.0, &right.0);
        self.parents.entry(left.0).or_default().push((pair, false));
        self.parents.entry(right.0).or_default().push((pair, true));
        self.activate(pair);
    }

    fn shortest_path(&self, hash: Bytes32, plain_length: u64) -> Option<Vec<u8>> {
        if plain_length < 4 {
            return None;
        }
        let max_atom_bytes = plain_length - 1;
        let max_steps: usize = max_atom_bytes
            .saturating_mul(8)
            .saturating_sub(1)
            .try_into()
            .unwrap_or(usize::MAX);

        let mut queue = VecDeque::from([(hash, Vec::new())]);
        let mut seen = HashSet::from([hash]);
        let mut candidates = Vec::new();
        let mut result_depth = None;
        while let Some((node, path)) = queue.pop_front() {
            if result_depth.is_some_and(|depth| path.len() > depth) {
                break;
            }
            if node == self.root {
                let encoded = encode_backref_path(&path);
                if atom_length_bits(path.len() as u64 + 1).is_some_and(|len| len <= max_atom_bytes)
                {
                    result_depth = Some(path.len());
                    candidates.push(encoded);
                }
                continue;
            }
            if path.len() >= max_steps {
                continue;
            }
            for &(parent, direction) in self.parents.get(&node).into_iter().flatten() {
                if self.active.get(&parent).copied().unwrap_or_default() == 0
                    || !seen.insert(parent)
                {
                    continue;
                }
                let mut next = path.clone();
                next.push(direction);
                queue.push_back((parent, next));
            }
        }
        candidates.into_iter().min()
    }
}

/// Post-order pass filling per-node tree-hash and (plain) serialized-length memos, keyed on node
/// address. Shared subtrees (equal address) are visited once. Iterative to avoid deep recursion on
/// long spend lists.
fn compute_memos(root: &SExp) -> (HashMap<usize, Bytes32>, HashMap<usize, u64>) {
    enum Op<'a> {
        Enter(&'a SExp<'a>),
        Exit(&'a SExp<'a>),
    }
    let mut hashes: HashMap<usize, Bytes32> = HashMap::new();
    let mut lens: HashMap<usize, u64> = HashMap::new();
    let mut stack: Vec<Op> = vec![Op::Enter(root)];
    while let Some(op) = stack.pop() {
        match op {
            Op::Enter(node) => {
                let key = addr(node);
                if hashes.contains_key(&key) {
                    continue;
                }
                match node {
                    SExp::Atom(atom) => {
                        hashes.insert(key, atom_tree_hash(atom.as_ref()));
                        lens.insert(key, serialized_length_atom(atom.as_ref()));
                    }
                    SExp::Pair(pair) => {
                        stack.push(Op::Exit(node));
                        stack.push(Op::Enter(pair.first()));
                        stack.push(Op::Enter(pair.rest()));
                    }
                }
            }
            Op::Exit(node) => {
                if let SExp::Pair(pair) = node {
                    let key = addr(node);
                    let lh = hashes[&addr(pair.first())];
                    let rh = hashes[&addr(pair.rest())];
                    hashes.insert(key, pair_tree_hash(&lh, &rh));
                    let ll = lens[&addr(pair.first())];
                    let rl = lens[&addr(pair.rest())];
                    lens.insert(key, 1u64.saturating_add(ll).saturating_add(rl));
                }
            }
        }
    }
    (hashes, lens)
}

fn node_to_stream_backrefs<W: Write>(root: &SExp, f: &mut W) -> io::Result<()> {
    #[derive(PartialEq, Eq)]
    enum ReadOp {
        Parse,
        Cons,
    }
    let (hashes, lens) = compute_memos(root);
    let mut read_op_stack: Vec<ReadOp> = vec![ReadOp::Parse];
    let mut write_stack: Vec<&SExp> = vec![root];
    let mut stack_index = BackrefStackIndex::new();

    while let Some(node) = write_stack.pop() {
        let op = read_op_stack.pop();
        debug_assert!(op == Some(ReadOp::Parse));
        let key = addr(node);
        let node_hash = hashes[&key];
        let node_len = lens[&key];
        match stack_index.shortest_path(node_hash, node_len) {
            Some(path) => {
                f.write_all(&[BACK_REFERENCE])?;
                write_atom(f, &path)?;
                stack_index.activate(node_hash);
            }
            None => match node {
                SExp::Pair(pair) => {
                    f.write_all(&[CONS_BOX_MARKER])?;
                    write_stack.push(pair.rest());
                    write_stack.push(pair.first());
                    read_op_stack.push(ReadOp::Cons);
                    read_op_stack.push(ReadOp::Parse);
                    read_op_stack.push(ReadOp::Parse);
                }
                SExp::Atom(atom) => {
                    write_atom(f, atom.as_ref())?;
                    stack_index.activate(node_hash);
                }
            },
        }
        while let Some(ReadOp::Cons) = read_op_stack.last() {
            read_op_stack.pop();
            stack_index.combine_top_pair();
        }
    }
    Ok(())
}

/// Serialize `sexp` with CLVM back-reference compression.
///
/// # Errors
/// Propagates the underlying `io::Error` (an in-memory `Cursor` write only fails on allocation).
pub fn sexp_to_bytes_backrefs(sexp: &SExp) -> io::Result<SerializedProgram> {
    let mut buffer = Cursor::new(Vec::new());
    node_to_stream_backrefs(sexp, &mut buffer)?;
    Ok(buffer.into_inner().into())
}

#[cfg(test)]
#[path = "../../tests/unit/clvm/serialize_backrefs/tests.rs"]
mod tests;
