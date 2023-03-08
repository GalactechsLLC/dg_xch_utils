use lazy_static::lazy_static;
use crate::clvm::utils::hash_256;
use crate::types::blockchain::sized_bytes::Bytes32;

pub const NULL: [u8; 0] = [];
pub const ONE: [u8; 1] = [0x01];
pub const TWO: [u8; 1] = [0x02];
pub const Q_KW: [u8; 1] = [0x01];
pub const A_KW: [u8; 1] = [0x02];
pub const C_KW: [u8; 1] = [0x04];

pub fn shatree_atom(atom: &[u8]) -> Bytes32 {
    Bytes32::from(hash_256(ONE.iter().copied().chain(atom.iter().copied()).collect::<Vec<u8>>()))
}

pub fn shatree_pair(left_hash: &Bytes32, right_hash: &Bytes32) -> Bytes32 {
    Bytes32::from(hash_256(TWO.iter().copied().chain(left_hash.to_sized_bytes().iter().copied().chain(right_hash.to_sized_bytes().iter().copied())).collect::<Vec<u8>>()))
}

lazy_static! {
    pub static ref Q_KW_TREEHASH: Bytes32 = shatree_atom(&Q_KW);
    pub static ref A_KW_TREEHASH: Bytes32 = shatree_atom(&A_KW);
    pub static ref C_KW_TREEHASH: Bytes32 = shatree_atom(&C_KW);
    pub static ref ONE_TREEHASH: Bytes32 = shatree_atom(&ONE);
    pub static ref NULL_TREEHASH: Bytes32 = shatree_atom(&NULL);
}

pub fn curried_values_tree_hash(arguments: &[Bytes32]) -> Bytes32 {
    if arguments.is_empty() {
        ONE_TREEHASH.clone()
    } else {
        shatree_pair(
            &C_KW_TREEHASH,
            &shatree_pair(
                &shatree_pair(&Q_KW_TREEHASH, &arguments[0]),
                &shatree_pair(&curried_values_tree_hash(&arguments[1..]), &NULL_TREEHASH),
            ),
        )
    }
}


pub fn curry_and_treehash(hash_of_quoted_mod_hash: &Bytes32, hashed_arguments: &[Bytes32]) -> Bytes32 {
    let curried_values = curried_values_tree_hash(hashed_arguments);
    return shatree_pair(
        &A_KW_TREEHASH,
        &shatree_pair(&hash_of_quoted_mod_hash, &shatree_pair(&curried_values, &NULL_TREEHASH)),
    )
}

pub fn calculate_hash_of_quoted_mod_hash(mod_hash: &Bytes32) -> Bytes32 {
    shatree_pair(&Q_KW_TREEHASH, mod_hash)
}