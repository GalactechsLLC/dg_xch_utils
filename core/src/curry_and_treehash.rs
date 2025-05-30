use crate::blockchain::sized_bytes::Bytes32;
use crate::utils::hash_256;
use once_cell::sync::Lazy;

pub const NULL: [u8; 0] = [];
pub const ONE: [u8; 1] = [0x01];
pub const TWO: [u8; 1] = [0x02];
pub const Q_KW: [u8; 1] = [0x01];
pub const A_KW: [u8; 1] = [0x02];
pub const C_KW: [u8; 1] = [0x04];

#[must_use]
pub fn shatree_atom(atom: &[u8]) -> Bytes32 {
    hash_256([ONE.as_slice(), atom].concat()).into()
}

#[must_use]
pub fn shatree_pair(left_hash: &Bytes32, right_hash: &Bytes32) -> Bytes32 {
    hash_256([TWO.as_slice(), left_hash.as_ref(), right_hash.as_ref()].concat()).into()
}

pub static Q_KW_TREEHASH: Lazy<Bytes32> = Lazy::new(|| shatree_atom(&Q_KW));
pub static A_KW_TREEHASH: Lazy<Bytes32> = Lazy::new(|| shatree_atom(&A_KW));
pub static C_KW_TREEHASH: Lazy<Bytes32> = Lazy::new(|| shatree_atom(&C_KW));
pub static ONE_TREEHASH: Lazy<Bytes32> = Lazy::new(|| shatree_atom(&ONE));
pub static NULL_TREEHASH: Lazy<Bytes32> = Lazy::new(|| shatree_atom(&NULL));

#[must_use]
pub fn curried_values_tree_hash(arguments: &[Bytes32]) -> Bytes32 {
    if arguments.is_empty() {
        *ONE_TREEHASH
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

#[must_use]
pub fn curry_and_treehash(
    hash_of_quoted_mod_hash: &Bytes32,
    hashed_arguments: &[Bytes32],
) -> Bytes32 {
    let curried_values = curried_values_tree_hash(hashed_arguments);
    shatree_pair(
        &A_KW_TREEHASH,
        &shatree_pair(
            hash_of_quoted_mod_hash,
            &shatree_pair(&curried_values, &NULL_TREEHASH),
        ),
    )
}

#[must_use]
pub fn calculate_hash_of_quoted_mod_hash(mod_hash: &Bytes32) -> Bytes32 {
    shatree_pair(&Q_KW_TREEHASH, mod_hash)
}
