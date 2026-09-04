//! Compact CLVM node arena — u32 handles into typed pools. A handle carries a 6-bit object
//! tag over a 26-bit index; small positive integers are encoded inline in the handle, with
//! ghost accounting so the inline optimization cannot change the consensus atom/pair limits.

use crate::clvm::sexp::{AtomBuf, PairBuf, SExp};
use crate::clvm::sexp_ext::SExpNumber;
use crate::errors::ClvmError;
use crate::formatting::{bigint_to_bytes, number_from_slice};
use num_bigint::{BigInt, Sign};
use std::fmt;
use std::sync::Arc;

// consensus limits
const MAX_NUM_ATOMS: usize = 62_500_000;
const MAX_NUM_PAIRS: usize = 62_500_000;
const NODE_PTR_IDX_BITS: u32 = 26;
const NODE_PTR_IDX_MASK: u32 = (1 << NODE_PTR_IDX_BITS) - 1;
// Handles are 32-bit; the byte heap is addressed by u32 spans.
const HEAP_LIMIT: usize = u32::MAX as usize;

/// A compact handle to a node in an [`Arena`]. The top 6 bits carry the object type, the low
/// 26 bits an index (pair pool, atom pool) or the small-atom value itself.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodePtr(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    /// Low bits index `pair_vec`.
    Pair,
    /// Low bits index `atom_vec`.
    Bytes,
    /// Low bits are the atom value itself (canonical unsigned integer, ≤ 26 bits).
    SmallAtom,
}

impl NodePtr {
    pub const NIL: Self = Self::new(ObjectType::SmallAtom, 0);
    pub const ONE: Self = Self::new(ObjectType::SmallAtom, 1);

    #[allow(clippy::cast_possible_truncation)]
    const fn new(object_type: ObjectType, index: usize) -> Self {
        debug_assert!(index <= NODE_PTR_IDX_MASK as usize);
        NodePtr(((object_type as u32) << NODE_PTR_IDX_BITS) | (index as u32))
    }

    #[must_use]
    pub fn object_type(self) -> ObjectType {
        match self.0 >> NODE_PTR_IDX_BITS {
            0 => ObjectType::Pair,
            1 => ObjectType::Bytes,
            2 => ObjectType::SmallAtom,
            _ => unreachable!(),
        }
    }

    #[must_use]
    pub fn index(self) -> u32 {
        self.0 & NODE_PTR_IDX_MASK
    }

    #[must_use]
    pub fn is_atom(self) -> bool {
        !self.is_pair()
    }

    #[must_use]
    pub fn is_pair(self) -> bool {
        self.object_type() == ObjectType::Pair
    }
}

impl Default for NodePtr {
    fn default() -> Self {
        Self::NIL
    }
}

impl fmt::Debug for NodePtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("NodePtr")
            .field(&self.object_type())
            .field(&self.index())
            .finish()
    }
}

/// Shape of a node: an atom, or a pair of child handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Atom,
    Pair(NodePtr, NodePtr),
}

/// Atom bytes: borrowed from the arena heap, or the canonical encoding of an inline small
/// atom materialized into a 4-byte buffer.
#[derive(Debug, Clone, Copy)]
pub enum Atom<'a> {
    Borrowed(&'a [u8]),
    U32([u8; 4], usize),
}

impl AsRef<[u8]> for Atom<'_> {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::U32(bytes, len) => &bytes[4 - len..],
        }
    }
}

impl Atom<'_> {
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Borrowed(bytes) => bytes.len(),
            Self::U32(_, len) => *len,
        }
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Copy, Debug)]
struct AtomSpan {
    start: u32,
    end: u32,
}

impl AtomSpan {
    fn len(&self) -> usize {
        (self.end - self.start) as usize
    }
}

#[derive(Clone, Copy, Debug)]
struct IntPair {
    first: NodePtr,
    rest: NodePtr,
}

/// Returns the inline value if `v` is the canonical encoding of an unsigned integer that
/// fits in 26 bits.
#[must_use]
pub fn fits_in_small_atom(v: &[u8]) -> Option<u32> {
    if !v.is_empty()
        && (v.len() > 4
        || (v.len() == 1 && v[0] == 0)
        // a 1-byte buffer of 0 is not the canonical representation of 0
        || (v[0] & 0x80) != 0
        // if the top bit is set, it's a negative number (i.e. not positive)
        || (v[0] == 0 && (v[1] & 0x80) == 0)
        // a leading zero is only canonical when it protects a set high bit
        || (v.len() == 4 && v[0] > 0x03))
    {
        None
    } else {
        let mut ret: u32 = 0;
        for b in v {
            ret <<= 8;
            ret |= u32::from(*b);
        }
        Some(ret)
    }
}

/// Length of the canonical encoding of a small-atom value.
#[must_use]
pub fn len_for_value(val: u32) -> usize {
    if val == 0 {
        0
    } else if val < 0x80 {
        1
    } else if val < 0x8000 {
        2
    } else if val < 0x0080_0000 {
        3
    } else if val < 0x8000_0000 {
        4
    } else {
        5
    }
}

/// The compact node store. All eval-time allocation goes through this; `reset` truncates the
/// pools without releasing capacity, so a reused runtime performs no steady-state mallocs.
/// Sharing only pays when the map entry costs less than the storage it saves. A cons cell is
/// 8 bytes against a ~32-byte entry, so pairs are never interned — measured, it cost 52 MiB of
/// peak on a 532-spend block to save nothing. Atoms repay it only when the payload dominates.
const INTERN_MIN_ATOM_BYTES: usize = 32;

pub struct Arena {
    // Identical atom bodies share one span. Keyed by the bytes; inline small atoms are excluded
    // since they already cost no storage.
    atom_intern: std::collections::HashMap<Box<[u8]>, NodePtr>,
    // grow-only byte heap for atom contents (atoms are immutable once created)
    u8_vec: Vec<u8>,
    // pair pool — 8 bytes per cons cell
    pair_vec: Vec<IntPair>,
    // atom pool — (start, end) spans into u8_vec
    atom_vec: Vec<AtomSpan>,
    // ghost counters account for atoms/pairs/heap-bytes that were optimized out (inline
    // small atoms, zero-copy substr/concat shortcuts) so the consensus limits are unchanged
    ghost_atoms: usize,
    ghost_pairs: usize,
    ghost_heap: usize,
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

/// A point the pools can be rewound to. Only meaningful while no node allocated after it is
/// still reachable — see [`Arena::restore`].
#[derive(Clone, Copy)]
pub struct Checkpoint {
    heap: usize,
    pairs: usize,
    atoms: usize,
}

impl Arena {
    #[must_use]
    pub fn new() -> Self {
        let mut arena = Self {
            atom_intern: std::collections::HashMap::new(),
            u8_vec: Vec::new(),
            pair_vec: Vec::new(),
            atom_vec: Vec::new(),
            ghost_atoms: 0,
            ghost_pairs: 0,
            ghost_heap: 0,
        };
        arena.u8_vec.reserve(1024 * 1024);
        arena.atom_vec.reserve(256);
        arena.pair_vec.reserve(256);
        arena.reset();
        arena
    }

    /// Record the current pool sizes so a later [`Arena::restore`] can discard everything
    /// allocated since.
    #[must_use]
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            heap: self.u8_vec.len(),
            pairs: self.pair_vec.len(),
            atoms: self.atom_vec.len(),
        }
    }

    /// Discard everything allocated since `cp`. Every `NodePtr` handed out after it is invalid:
    /// indices are reused, so a stale handle silently denotes a different node. The caller must
    /// know nothing allocated after the checkpoint is reachable.
    ///
    /// Rewound storage converts into ghosts rather than falling out of the count: the ceilings
    /// are consensus, and the reference allocator only grows within a run, so a node minted and
    /// rewound must still count against them. The intern map is cleared wholesale — its entries
    /// index into the truncated pools.
    pub fn restore(&mut self, cp: Checkpoint) {
        self.ghost_heap += self.u8_vec.len() - cp.heap;
        self.ghost_pairs += self.pair_vec.len() - cp.pairs;
        self.ghost_atoms += self.atom_vec.len() - cp.atoms;
        self.u8_vec.truncate(cp.heap);
        self.pair_vec.truncate(cp.pairs);
        self.atom_vec.truncate(cp.atoms);
        self.atom_intern.clear();
    }

    /// Truncate all pools (capacity retained) and reset the ghost counters to their initial
    /// state (2 ghost atoms + 1 ghost heap byte, standing in for nil/one).
    pub fn reset(&mut self) {
        self.atom_intern.clear();
        self.u8_vec.clear();
        self.pair_vec.clear();
        self.atom_vec.clear();
        self.ghost_atoms = 2;
        self.ghost_pairs = 0;
        self.ghost_heap = 1;
    }

    fn check_atom_limit(&self) -> Result<(), ClvmError> {
        if self.atom_vec.len() + self.ghost_atoms == MAX_NUM_ATOMS {
            Err(ClvmError::TooManyAtoms)
        } else {
            Ok(())
        }
    }

    /// Atoms materialized in the heap; excludes inline small atoms.
    #[must_use]
    pub fn stored_atom_count(&self) -> usize {
        self.atom_vec.len()
    }

    /// Pairs materialized in the pool.
    #[must_use]
    pub fn stored_pair_count(&self) -> usize {
        self.pair_vec.len()
    }

    /// Bytes held in the atom heap.
    #[must_use]
    pub fn stored_heap_bytes(&self) -> usize {
        self.u8_vec.len()
    }

    pub fn new_atom(&mut self, v: &[u8]) -> Result<NodePtr, ClvmError> {
        let start = self.u8_vec.len();
        if start + self.ghost_heap + v.len() > HEAP_LIMIT {
            return Err(ClvmError::OutOfMemory);
        }
        self.check_atom_limit()?;
        if let Some(val) = fits_in_small_atom(v) {
            self.ghost_atoms += 1;
            self.ghost_heap += v.len();
            Ok(NodePtr::new(ObjectType::SmallAtom, val as usize))
        } else {
            // A hit still charges the ghost counters: the consensus ceilings count every logical
            // atom, stored or shared.
            if v.len() >= INTERN_MIN_ATOM_BYTES
                && let Some(&existing) = self.atom_intern.get(v)
            {
                self.ghost_atoms += 1;
                self.ghost_heap += v.len();
                return Ok(existing);
            }
            let idx = self.atom_vec.len();
            self.u8_vec.extend_from_slice(v);
            #[allow(clippy::cast_possible_truncation)]
            self.atom_vec.push(AtomSpan {
                start: start as u32,
                end: self.u8_vec.len() as u32,
            });
            let node = NodePtr::new(ObjectType::Bytes, idx);
            if v.len() >= INTERN_MIN_ATOM_BYTES {
                self.atom_intern.insert(v.into(), node);
            }
            Ok(node)
        }
    }

    pub fn new_pair(&mut self, first: NodePtr, rest: NodePtr) -> Result<NodePtr, ClvmError> {
        let idx = self.pair_vec.len();
        if idx >= MAX_NUM_PAIRS - self.ghost_pairs {
            return Err(ClvmError::TooManyPairs);
        }
        self.pair_vec.push(IntPair { first, rest });
        Ok(NodePtr::new(ObjectType::Pair, idx))
    }

    /// Zero-copy substring view of an atom: a new span into the parent's bytes for pool
    /// atoms; re-encode (and re-inline when canonical) for small atoms.
    pub fn new_substr(
        &mut self,
        node: NodePtr,
        start: u32,
        end: u32,
    ) -> Result<NodePtr, ClvmError> {
        self.check_atom_limit()?;
        fn bounds_check(start: u32, end: u32, len: u32) -> Result<(), ClvmError> {
            if start > len {
                return Err(ClvmError::InvalidInput(format!(
                    "substr start out of bounds: {start} is > {len}"
                )));
            }
            if end > len {
                return Err(ClvmError::InvalidInput(format!(
                    "substr end out of bounds: {end} is > {len}"
                )));
            }
            if end < start {
                return Err(ClvmError::InvalidInput(format!(
                    "substr invalid bounds: {start} is > {end}"
                )));
            }
            Ok(())
        }
        match node.object_type() {
            ObjectType::Pair => Err(ClvmError::ExpectedAtomGotPair("substr on pair".to_string())),
            ObjectType::Bytes => {
                let atom = self.atom_vec[node.index() as usize];
                bounds_check(start, end, atom.end - atom.start)?;
                let idx = self.atom_vec.len();
                self.atom_vec.push(AtomSpan {
                    start: atom.start + start,
                    end: atom.start + end,
                });
                Ok(NodePtr::new(ObjectType::Bytes, idx))
            }
            ObjectType::SmallAtom => {
                let val = node.index();
                #[allow(clippy::cast_possible_truncation)]
                let len = len_for_value(val) as u32;
                bounds_check(start, end, len)?;
                let buf: [u8; 4] = val.to_be_bytes();
                let buf = &buf[4 - len as usize..];
                let substr = &buf[start as usize..end as usize];
                if let Some(new_val) = fits_in_small_atom(substr) {
                    self.ghost_atoms += 1;
                    Ok(NodePtr::new(ObjectType::SmallAtom, new_val as usize))
                } else {
                    let heap_start = self.u8_vec.len();
                    let owned = substr.to_vec();
                    self.u8_vec.extend_from_slice(&owned);
                    let idx = self.atom_vec.len();
                    #[allow(clippy::cast_possible_truncation)]
                    self.atom_vec.push(AtomSpan {
                        start: heap_start as u32,
                        end: self.u8_vec.len() as u32,
                    });
                    Ok(NodePtr::new(ObjectType::Bytes, idx))
                }
            }
        }
    }

    /// Concatenation into a single fresh atom, including the zero- and one-term ghost
    /// shortcuts, which keep allocation counters consensus-exact.
    pub fn new_concat(&mut self, new_size: usize, nodes: &[NodePtr]) -> Result<NodePtr, ClvmError> {
        self.check_atom_limit()?;
        let start = self.u8_vec.len();
        if start + self.ghost_heap + new_size > HEAP_LIMIT {
            return Err(ClvmError::OutOfMemory);
        }
        if nodes.is_empty() {
            if new_size != 0 {
                return Err(ClvmError::InvalidInput(
                    "concat passed invalid new_size".to_string(),
                ));
            }
            self.ghost_atoms += 1;
            return Ok(NodePtr::NIL);
        }
        if nodes.len() == 1 {
            let Some(len) = self.atom_len(nodes[0]) else {
                return Err(ClvmError::ExpectedAtomGotPair("concat on pair".to_string()));
            };
            if len != new_size {
                return Err(ClvmError::InvalidInput(
                    "concat passed invalid new_size".to_string(),
                ));
            }
            self.ghost_heap += new_size;
            self.ghost_atoms += 1;
            return Ok(nodes[0]);
        }
        self.u8_vec.reserve(new_size);
        let mut counter: usize = 0;
        for node in nodes {
            match node.object_type() {
                ObjectType::Pair => {
                    self.u8_vec.truncate(start);
                    return Err(ClvmError::ExpectedAtomGotPair("concat on pair".to_string()));
                }
                ObjectType::Bytes => {
                    let term = self.atom_vec[node.index() as usize];
                    if counter + term.len() > new_size {
                        self.u8_vec.truncate(start);
                        return Err(ClvmError::InvalidInput(
                            "concat passed invalid new_size".to_string(),
                        ));
                    }
                    self.u8_vec
                        .extend_from_within(term.start as usize..term.end as usize);
                    counter += term.len();
                }
                ObjectType::SmallAtom => {
                    let val = node.index();
                    let len = len_for_value(val);
                    let buf: [u8; 4] = val.to_be_bytes();
                    self.u8_vec.extend_from_slice(&buf[4 - len..]);
                    counter += len;
                }
            }
        }
        if counter != new_size {
            self.u8_vec.truncate(start);
            return Err(ClvmError::InvalidInput(
                "concat passed invalid new_size".to_string(),
            ));
        }
        let idx = self.atom_vec.len();
        #[allow(clippy::cast_possible_truncation)]
        self.atom_vec.push(AtomSpan {
            start: start as u32,
            end: self.u8_vec.len() as u32,
        });
        Ok(NodePtr::new(ObjectType::Bytes, idx))
    }

    /// Encode a number as a fresh atom with the crate's minimal signed big-endian encoding.
    pub fn new_number(&mut self, n: &SExpNumber) -> Result<NodePtr, ClvmError> {
        match n {
            SExpNumber::I128(v) => self.new_i128(*v),
            SExpNumber::BigInt(b) => self.new_bigint(b),
        }
    }

    pub fn new_i128(&mut self, v: i128) -> Result<NodePtr, ClvmError> {
        if v == 0 {
            return self.new_atom(&[]);
        }
        let raw = v.to_be_bytes();
        let mut s: &[u8] = raw.as_slice();
        while s.len() > 1 && s[0] == (u8::from(s[1] & 0x80 > 0) * 0xFF) {
            s = &s[1..];
        }
        self.new_atom(s)
    }

    pub fn new_bigint(&mut self, v: &BigInt) -> Result<NodePtr, ClvmError> {
        let bytes = bigint_to_bytes(v, v.sign() != Sign::NoSign);
        self.new_atom(&bytes)
    }

    /// The atom bytes for `node`, or `None` if it is a pair.
    #[must_use]
    pub fn atom(&self, node: NodePtr) -> Option<Atom<'_>> {
        match node.object_type() {
            ObjectType::Bytes => {
                let atom = self.atom_vec[node.index() as usize];
                Some(Atom::Borrowed(
                    &self.u8_vec[atom.start as usize..atom.end as usize],
                ))
            }
            ObjectType::SmallAtom => {
                let val = node.index();
                Some(Atom::U32(val.to_be_bytes(), len_for_value(val)))
            }
            ObjectType::Pair => None,
        }
    }

    /// The atom length for `node`, or `None` if it is a pair.
    #[must_use]
    pub fn atom_len(&self, node: NodePtr) -> Option<usize> {
        match node.object_type() {
            ObjectType::Bytes => Some(self.atom_vec[node.index() as usize].len()),
            ObjectType::SmallAtom => Some(len_for_value(node.index())),
            ObjectType::Pair => None,
        }
    }

    /// Signed-integer decode of an atom (same semantics as `SExpNumber::from(&AtomBuf)`),
    /// or `None` for a pair.
    #[must_use]
    pub fn number(&self, node: NodePtr) -> Option<SExpNumber> {
        match node.object_type() {
            ObjectType::SmallAtom => Some(SExpNumber::I128(i128::from(node.index()))),
            ObjectType::Bytes => {
                let atom = self.atom_vec[node.index() as usize];
                let buf = &self.u8_vec[atom.start as usize..atom.end as usize];
                Some(match buf.len() {
                    0 => SExpNumber::I128(0),
                    x if x <= 16 => {
                        let fill = if buf[0] & 0x80 != 0 { 0xff } else { 0x00 };
                        let mut int_buf = [fill; 16];
                        int_buf[(16 - x)..].copy_from_slice(buf);
                        SExpNumber::I128(i128::from_be_bytes(int_buf))
                    }
                    _ => SExpNumber::BigInt(number_from_slice(buf)),
                })
            }
            ObjectType::Pair => None,
        }
    }

    #[must_use]
    pub fn node_kind(&self, node: NodePtr) -> NodeKind {
        match node.object_type() {
            ObjectType::Pair => {
                let pair = self.pair_vec[node.index() as usize];
                NodeKind::Pair(pair.first, pair.rest)
            }
            ObjectType::Bytes | ObjectType::SmallAtom => NodeKind::Atom,
        }
    }

    /// `(first, rest)` if `node` is a pair.
    #[must_use]
    pub fn next(&self, node: NodePtr) -> Option<(NodePtr, NodePtr)> {
        match self.node_kind(node) {
            NodeKind::Pair(first, rest) => Some((first, rest)),
            NodeKind::Atom => None,
        }
    }

    #[must_use]
    pub fn nullp(&self, node: NodePtr) -> bool {
        node == NodePtr::NIL || matches!(self.atom_len(node), Some(0))
    }

    #[must_use]
    pub fn non_nil(&self, node: NodePtr) -> bool {
        !self.nullp(node)
    }

    #[must_use]
    pub fn arg_count(&self, node: NodePtr, return_early_if_exceeds: usize) -> usize {
        let mut count = 0;
        let mut ptr = node;
        while let Some((_, rest)) = self.next(ptr) {
            ptr = rest;
            count += 1;
            if count > return_early_if_exceeds {
                break;
            }
        }
        count
    }

    #[must_use]
    pub fn arg_count_is(&self, node: NodePtr, mut count: usize) -> bool {
        let mut ptr = node;
        loop {
            if count == 0 {
                return self.nullp(ptr);
            }
            match self.next(ptr) {
                Some((_, rest)) => ptr = rest,
                None => return false,
            }
            count -= 1;
        }
    }

    #[must_use]
    pub fn as_atom_list(&self, node: NodePtr) -> Vec<Vec<u8>> {
        let mut rtn: Vec<Vec<u8>> = Vec::new();
        let mut cur = node;
        while let Some((first, rest)) = self.next(cur) {
            match self.atom(first) {
                Some(a) => rtn.push(a.as_ref().to_vec()),
                None => return vec![],
            }
            cur = rest;
        }
        rtn
    }

    /// Deep-copy an owned/borrowed [`SExp`] tree into the arena. Iterative — a long CLVM
    /// list is deep in the `rest` direction and must not recurse.
    pub fn import(&mut self, sexp: &SExp) -> Result<NodePtr, ClvmError> {
        enum Job<'x> {
            Visit(&'x SExp<'x>),
            Build,
        }
        let mut jobs: Vec<Job> = vec![Job::Visit(sexp)];
        let mut out: Vec<NodePtr> = Vec::new();
        while let Some(job) = jobs.pop() {
            match job {
                Job::Visit(SExp::Atom(a)) => out.push(self.new_atom(a.as_ref())?),
                Job::Visit(SExp::Pair(p)) => {
                    jobs.push(Job::Build);
                    jobs.push(Job::Visit(p.rest()));
                    jobs.push(Job::Visit(p.first()));
                }
                Job::Build => {
                    let rest = out.pop().ok_or(ClvmError::ValueStackEmpty)?;
                    let first = out.pop().ok_or(ClvmError::ValueStackEmpty)?;
                    out.push(self.new_pair(first, rest)?);
                }
            }
        }
        out.pop().ok_or(ClvmError::ValueStackEmpty)
    }

    /// Materialize an arena subtree as an owned [`SExp`] tree. Iterative for the same
    /// reason as [`Arena::import`].
    #[must_use]
    pub fn export(&self, node: NodePtr) -> SExp<'static> {
        enum Job {
            Visit(NodePtr),
            Build,
        }
        let mut jobs: Vec<Job> = vec![Job::Visit(node)];
        let mut out: Vec<SExp<'static>> = Vec::new();
        while let Some(job) = jobs.pop() {
            match job {
                Job::Visit(ptr) => match self.node_kind(ptr) {
                    NodeKind::Atom => {
                        let bytes = self
                            .atom(ptr)
                            .expect("node_kind atom has bytes")
                            .as_ref()
                            .to_vec();
                        out.push(SExp::Atom(AtomBuf::new(bytes)));
                    }
                    NodeKind::Pair(first, rest) => {
                        jobs.push(Job::Build);
                        jobs.push(Job::Visit(rest));
                        jobs.push(Job::Visit(first));
                    }
                },
                Job::Build => {
                    let rest = out.pop().expect("build has rest");
                    let first = out.pop().expect("build has first");
                    out.push(SExp::Pair(PairBuf::Owned((
                        Arc::new(first),
                        Arc::new(rest),
                    ))));
                }
            }
        }
        out.pop().expect("export produced a node")
    }

    /// Render a subtree with the crate's canonical `SExp` `Display` — error paths only.
    #[must_use]
    pub fn display(&self, node: NodePtr) -> String {
        self.export(node).to_string()
    }

    /// Render a subtree with the crate's canonical `SExp` `Debug` — error paths only.
    #[must_use]
    pub fn debug_fmt(&self, node: NodePtr) -> String {
        format!("{:?}", self.export(node))
    }

    /// Allocation counters (allocated + ghost: atoms, pairs, heap bytes), for probes and
    /// limit diagnostics.
    #[must_use]
    pub fn counters(&self) -> (usize, usize, usize) {
        (
            self.atom_vec.len() + self.ghost_atoms,
            self.pair_vec.len() + self.ghost_pairs,
            self.u8_vec.len() + self.ghost_heap,
        )
    }
}

/// Argument-list cursor with the exact semantics of the tree walker's `SExpIter`: yields
/// each pair's `first`; at a NON-NIL terminal atom yields that atom itself once, then stops
/// (improper tails surface their tail); at nil stops. Holds no borrow between calls so ops
/// can allocate while iterating.
pub struct ArgCursor {
    cur: NodePtr,
    done: bool,
}

impl ArgCursor {
    #[must_use]
    pub fn new(args: NodePtr) -> Self {
        Self {
            cur: args,
            done: false,
        }
    }
    pub fn next(&mut self, arena: &Arena) -> Option<NodePtr> {
        if self.done {
            return None;
        }
        match arena.node_kind(self.cur) {
            NodeKind::Pair(first, rest) => {
                self.cur = rest;
                Some(first)
            }
            NodeKind::Atom => {
                self.done = true;
                if arena.non_nil(self.cur) {
                    Some(self.cur)
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/clvm/arena/tests.rs"]
mod tests;
