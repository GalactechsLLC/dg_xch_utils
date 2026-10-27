//! Minimal direct-Falcon wallet puzzle for development chains. The signature
//! binds both destination and amount, plus the spent coin and network (AGG_SIG_ME).
use super::{
    SignatureSigner,
    falcon::{FalconPublicKey, FalconSignature, FalconSigner},
    falcon_condition::transaction_reference,
};
use crate::{
    blockchain::{
        coin::Coin, coin_spend::CoinSpend, sized_bytes::Bytes32, spend_bundle::SpendBundle,
    },
    clvm::{program::Program, sexp::SExp},
    consensus::{block_generator::conditions_from_spend_bundle, constants::ConsensusConstants},
};
use std::io::Error;

fn quote(value: SExp<'static>) -> SExp<'static> {
    SExp::from(1u8).cons(value)
}
fn cons(first: SExp<'static>, rest: SExp<'static>) -> SExp<'static> {
    vec![SExp::from(4u8), first, rest].into()
}
fn nil() -> SExp<'static> {
    Vec::<u8>::new().into()
}

pub fn puzzle(public_key: &FalconPublicKey) -> Program<'static> {
    // Solution = (destination amount). Paths 2 and 5 select those two atoms.
    let message: SExp<'static> = vec![SExp::from(11u8), SExp::from(2u8), SExp::from(5u8)].into();
    let signature = cons(
        quote(50u8.into()),
        cons(
            quote(public_key.to_vec().into()),
            cons(message, quote(vec![nil()].into())),
        ),
    );
    let output = cons(
        quote(51u8.into()),
        cons(2u8.into(), cons(5u8.into(), quote(nil()))),
    );
    Program::new(cons(signature, cons(output, quote(nil()))))
}

pub struct FalconSpendBundle {
    pub bundle: SpendBundle,
    pub signatures: Vec<FalconSignature>,
}

pub fn spend(
    signer: &FalconSigner,
    coin: Coin,
    destination: Bytes32,
    amount: u64,
    constants: &ConsensusConstants,
    height: u32,
) -> Result<FalconSpendBundle, Error> {
    if constants.signature_profile.is_bls() {
        return Err(Error::other("Falcon wallet requires Falcon chain rules"));
    }
    let puzzle = puzzle(&signer.public_key());
    if coin.puzzle_hash != puzzle.tree_hash() || amount > coin.amount {
        return Err(Error::other(
            "coin does not belong to this Falcon wallet or amount exceeds balance",
        ));
    }
    let solution = Program::to(vec![
        SExp::from(destination.const_bytes().to_vec()),
        SExp::from(amount),
    ]);
    let mut bundle = SpendBundle {
        authorization: Default::default(),
        coin_spends: vec![CoinSpend {
            coin,
            puzzle_reveal: puzzle.serialized().map_err(Error::other)?,
            solution: solution.serialized().map_err(Error::other)?,
        }],
        ..SpendBundle::empty()
    };
    let conditions =
        conditions_from_spend_bundle(&bundle, height, constants).map_err(Error::other)?;
    let signatures = conditions
        .falcon_signatures
        .iter()
        .map(|requirement| {
            if requirement.public_key.const_bytes() != signer.public_key() {
                return Err(Error::other("unexpected wallet signing key"));
            }
            signer
                .sign_message(requirement.message.as_slice())
                .map_err(Error::other)
        })
        .collect::<Result<Vec<_>, _>>()?;
    bundle.aggregated_signature =
        transaction_reference(constants.genesis_challenge, &conditions.falcon_signatures);
    bundle.authorization = super::extension::FalconAuthorization {
        labrador: None,
        genesis: constants.genesis_challenge,
        header: Vec::new(),
        transactions: signatures.clone(),
    }
    .into();
    Ok(FalconSpendBundle { bundle, signatures })
}
