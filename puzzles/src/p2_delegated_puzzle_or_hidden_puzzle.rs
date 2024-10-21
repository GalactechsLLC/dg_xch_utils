use crate::p2_conditions::puzzle_for_conditions;
use blst::min_pk::{AggregatePublicKey, SecretKey};
use dg_xch_core::blockchain::sized_bytes::{hex_to_bytes, Bytes32, Bytes48, SizedBytes};
use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::clvm::sexp;
use dg_xch_core::clvm::sexp::IntoSExp;
use dg_xch_core::curry_and_treehash::{calculate_hash_of_quoted_mod_hash, curry_and_treehash};
use dg_xch_serialize::hash_256;
use lazy_static::lazy_static;
use num_bigint::BigInt;
use num_integer::Integer;
use std::io::{Error, ErrorKind};

const P2_DELEGATED_PUZZLE_OR_HIDDEN_PUZZLE_HEX: &str = "ff02ffff01ff02ffff03ff0bffff01ff02ffff03ffff09ff05ffff1dff0bffff1effff0bff0bffff02ff06ffff04ff02ffff04ff17ff8080808080808080ffff01ff02ff17ff2f80ffff01ff088080ff0180ffff01ff04ffff04ff04ffff04ff05ffff04ffff02ff06ffff04ff02ffff04ff17ff80808080ff80808080ffff02ff17ff2f808080ff0180ffff04ffff01ff32ff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff06ffff04ff02ffff04ff09ff80808080ffff02ff06ffff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff058080ff0180ff018080";

#[tokio::test]
pub async fn test_default_hash() {
    let serialized_program = SerializedProgram::from_hex("ff0980").unwrap();
    let default_hidden_puzzle = serialized_program.to_program();
    let default_hidden_hash = default_hidden_puzzle.tree_hash();
    assert_eq!(
        "0x711d6c4e32c92e53179b199484cf8c897542bc57f2b22582799f9d657eec4699".to_string(),
        default_hidden_hash.to_string()
    );
}

lazy_static! {
    pub static ref DEFAULT_HIDDEN_PUZZLE: Program =
        SerializedProgram::from_hex("ff0980").unwrap().to_program();
    pub static ref DEFAULT_HIDDEN_PUZZLE_HASH: Bytes32 = DEFAULT_HIDDEN_PUZZLE.tree_hash();
    pub static ref GROUP_ORDER: BigInt = BigInt::from_signed_bytes_be(
        &hex_to_bytes("0x73EDA753299D7D483339D80809A1D80553BDA402FFFE5BFEFFFFFFFF00000001")
            .unwrap()
    );
    pub static ref MOD: Program =
        SerializedProgram::from_hex(P2_DELEGATED_PUZZLE_OR_HIDDEN_PUZZLE_HEX)
            .unwrap()
            .to_program();
    pub static ref QUOTED_MOD_HASH: Bytes32 = calculate_hash_of_quoted_mod_hash(&MOD.tree_hash());
}

#[tokio::test]
pub async fn test_calculate_synthetic_offset() {
    let key = Bytes48::from("97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb");
    let result = calculate_synthetic_offset(&key, &DEFAULT_HIDDEN_PUZZLE_HASH);
    assert_eq!(
        "19134605735515143581103004370522950503760660832695882105316807119860397047163",
        format!("{result}")
    );
}

#[must_use]
pub fn calculate_synthetic_offset(public_key: &Bytes48, hidden_puzzle_hash: &Bytes32) -> BigInt {
    let mut to_hash = vec![];
    to_hash.extend(public_key.to_sized_bytes());
    to_hash.extend(hidden_puzzle_hash.to_sized_bytes());
    let blob = hash_256(to_hash);
    let offset = BigInt::from_signed_bytes_be(&blob);
    offset.mod_floor(&GROUP_ORDER)
}

pub fn calculate_synthetic_public_key(
    public_key: &Bytes48,
    hidden_puzzle_hash: &Bytes32,
) -> Result<Bytes48, Error> {
    let bytes = Bytes32::new(
        &calculate_synthetic_offset(public_key, hidden_puzzle_hash)
            .to_bytes_be()
            .1,
    );
    let synthetic_offset: SecretKey = bytes.into();
    let mut agg = AggregatePublicKey::from_public_key(&public_key.into());
    agg.add_public_key(&synthetic_offset.sk_to_pk(), false)
        .map_err(|e| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("Synthetic PK Error: {e:?}"),
            )
        })?;
    Ok(Bytes48::from(agg.to_public_key().to_bytes()))
}

pub fn calculate_synthetic_secret_key(
    secret_key: &SecretKey,
    hidden_puzzle_hash: &Bytes32,
) -> Result<SecretKey, Error> {
    let secret_exponent = BigInt::from_signed_bytes_be(&secret_key.to_bytes());
    let public_key = secret_key.sk_to_pk();
    let synthetic_offset =
        calculate_synthetic_offset(&public_key.to_bytes().into(), hidden_puzzle_hash);
    let synthetic_secret_exponent = (secret_exponent + synthetic_offset).mod_floor(&GROUP_ORDER);
    let blob = Bytes32::new(&synthetic_secret_exponent.to_bytes_be().1);
    SecretKey::from_bytes(blob.to_sized_bytes()).map_err(|e| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("Synthetic SK Error: {e:?}"),
        )
    })
}

pub fn puzzle_for_synthetic_public_key(synthetic_public_key: &Bytes48) -> Result<Program, Error> {
    MOD.curry(&[Program::try_from(synthetic_public_key)?])
}

pub fn puzzle_hash_for_synthetic_public_key(
    synthetic_public_key: &Bytes48,
) -> Result<Bytes32, Error> {
    let public_key_hash = Program::try_from(synthetic_public_key)?.tree_hash();
    Ok(curry_and_treehash(&QUOTED_MOD_HASH, &[public_key_hash]))
}

pub fn puzzle_for_public_key_and_hidden_puzzle_hash(
    public_key: &Bytes48,
    hidden_puzzle_hash: &Bytes32,
) -> Result<Program, Error> {
    let synthetic_public_key = calculate_synthetic_public_key(public_key, hidden_puzzle_hash)?;
    puzzle_for_synthetic_public_key(&synthetic_public_key)
}

pub fn puzzle_hash_for_public_key_and_hidden_puzzle_hash(
    public_key: &Bytes48,
    hidden_puzzle_hash: &Bytes32,
) -> Result<Bytes32, Error> {
    let synthetic_public_key = calculate_synthetic_public_key(public_key, hidden_puzzle_hash)?;
    puzzle_hash_for_synthetic_public_key(&synthetic_public_key)
}

pub fn puzzle_for_public_key_and_hidden_puzzle(
    public_key: &Bytes48,
    hidden_puzzle: &Program,
) -> Result<Program, Error> {
    puzzle_for_public_key_and_hidden_puzzle_hash(public_key, &hidden_puzzle.tree_hash())
}

pub fn puzzle_for_pk(public_key: &Bytes48) -> Result<Program, Error> {
    puzzle_for_public_key_and_hidden_puzzle_hash(public_key, &DEFAULT_HIDDEN_PUZZLE_HASH)
}

pub fn puzzle_hash_for_pk(public_key: &Bytes48) -> Result<Bytes32, Error> {
    puzzle_hash_for_public_key_and_hidden_puzzle_hash(public_key, &DEFAULT_HIDDEN_PUZZLE_HASH)
}

#[tokio::test]
pub async fn test_puzzle_hash_for_pk() {
    let key = Bytes48::from("97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb");
    let expected_puzzlehash =
        Bytes32::from("48068eb6150f738fe90a001c562f0c4b769b7d64a59915aa8c0886b978e38137");
    let result = puzzle_hash_for_pk(&key).unwrap();
    assert_eq!(expected_puzzlehash, result);
}

pub fn solution_for_delegated_puzzle(delegated_puzzle: Program, solution: Program) -> Program {
    Program::to(vec![
        sexp::NULL.clone(),
        delegated_puzzle.to_sexp(),
        solution.to_sexp(),
    ])
}

#[must_use]
pub fn solution_for_hidden_puzzle(
    hidden_public_key: Bytes48,
    hidden_puzzle: Program,
    solution_to_hidden_puzzle: Program,
) -> Program {
    Program::to(vec![
        hidden_public_key.to_sexp(),
        hidden_puzzle.to_sexp(),
        solution_to_hidden_puzzle.to_sexp(),
    ])
}

pub fn solution_for_conditions<T: IntoSExp>(conditions: T) -> Result<Program, Error> {
    let delegated_puzzle = puzzle_for_conditions(conditions)?;
    Ok(solution_for_delegated_puzzle(
        delegated_puzzle,
        Program::to(0),
    ))
}
