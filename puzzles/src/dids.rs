use dg_parser_macro::parse_program_hex;

parse_program_hex!(
    DID_INNERPUZ,
    "ff02ffff01ff02ffff03ff81bfffff01ff02ff05ff82017f80ffff01ff02ffff03ffff22ffff09ffff02ff7effff04ff02ffff04ff8217ffff80808080ff0b80ffff15ff17ff808080ffff01ff04ffff04ff28ffff04ff82017fff808080ffff04ffff04ff34ffff04ff8202ffffff04ff82017fffff04ffff04ff8202ffff8080ff8080808080ffff04ffff04ff38ffff04ff822fffff808080ffff02ff26ffff04ff02ffff04ff2fffff04ff17ffff04ff8217ffffff04ff822fffffff04ff8202ffffff04ff8205ffffff04ff820bffffff01ff8080808080808080808080808080ffff01ff088080ff018080ff0180ffff04ffff01ffffffff313dff4946ffff0233ff3c04ffffff0101ff02ff02ffff03ff05ffff01ff02ff3affff04ff02ffff04ff0dffff04ffff0bff2affff0bff22ff3c80ffff0bff2affff0bff2affff0bff22ff3280ff0980ffff0bff2aff0bffff0bff22ff8080808080ff8080808080ffff010b80ff0180ffffff02ffff03ff17ffff01ff02ffff03ff82013fffff01ff04ffff04ff30ffff04ffff0bffff0bffff02ff36ffff04ff02ffff04ff05ffff04ff27ffff04ff82023fffff04ff82053fffff04ff820b3fff8080808080808080ffff02ff7effff04ff02ffff04ffff02ff2effff04ff02ffff04ff2fffff04ff5fffff04ff82017fff808080808080ff8080808080ff2f80ff808080ffff02ff26ffff04ff02ffff04ff05ffff04ff0bffff04ff37ffff04ff2fffff04ff5fffff04ff8201bfffff04ff82017fffff04ffff10ff8202ffffff010180ff808080808080808080808080ffff01ff02ff26ffff04ff02ffff04ff05ffff04ff37ffff04ff2fffff04ff5fffff04ff8201bfffff04ff82017fffff04ff8202ffff8080808080808080808080ff0180ffff01ff02ffff03ffff15ff8202ffffff11ff0bffff01018080ffff01ff04ffff04ff20ffff04ff82017fffff04ff5fff80808080ff8080ffff01ff088080ff018080ff0180ff0bff17ffff02ff5effff04ff02ffff04ff09ffff04ff2fffff04ffff02ff7effff04ff02ffff04ffff04ff09ffff04ff0bff1d8080ff80808080ff808080808080ff5f80ffff04ffff0101ffff04ffff04ff2cffff04ff05ff808080ffff04ffff04ff20ffff04ff17ffff04ff0bff80808080ff80808080ffff0bff2affff0bff22ff2480ffff0bff2affff0bff2affff0bff22ff3280ff0580ffff0bff2affff02ff3affff04ff02ffff04ff07ffff04ffff0bff22ff2280ff8080808080ffff0bff22ff8080808080ff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff7effff04ff02ffff04ff09ff80808080ffff02ff7effff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff058080ff0180ff018080"
);

#[test]
pub fn test_hashes() {
    assert_eq!(
        dg_xch_core::blockchain::sized_bytes::Bytes32::const_hex(
            "33143d2bef64f14036742673afd158126b94284b4530a28c354fac202b0c910e"
        ),
        DID_INNERPUZ_TREE_HASH
    );
}

use crate::programs;
use dg_xch_core::blockchain::{coin::Coin, coin_spend::CoinSpend, sized_bytes::Bytes32};
use dg_xch_core::clvm::{program::Program, sexp::SExp};
use dg_xch_core::traits::SizedBytes;
use std::io::Error;

#[derive(Clone)]
pub struct DidInfo {
    pub launcher_id: Bytes32,
    pub recovery_list_hash: Option<Bytes32>,
    pub verifications_required: u64,
    pub metadata: Program<'static>,
}

impl DidInfo {
    fn singleton(&self) -> Program<'static> {
        Program::to((
            programs::SINGLETON_TOP_LAYER_V1_1_TREE_HASH,
            (self.launcher_id, programs::SINGLETON_LAUNCHER_TREE_HASH),
        ))
    }

    pub fn inner_puzzle_hash(&self, owner: Bytes32) -> Bytes32 {
        use dg_xch_core::curry_and_treehash::{
            calculate_hash_of_quoted_mod_hash, curry_and_treehash,
        };
        let recovery = self
            .recovery_list_hash
            .map_or_else(|| Program::to(0), Program::to);
        curry_and_treehash(
            &calculate_hash_of_quoted_mod_hash(&programs::DID_INNERPUZ_TREE_HASH),
            &[
                owner,
                recovery.tree_hash(),
                Program::to(self.verifications_required).tree_hash(),
                self.singleton().tree_hash(),
                self.metadata.tree_hash(),
            ],
        )
    }

    pub fn puzzle_hash(&self, owner: Bytes32) -> Bytes32 {
        use dg_xch_core::curry_and_treehash::{
            calculate_hash_of_quoted_mod_hash, curry_and_treehash,
        };
        curry_and_treehash(
            &calculate_hash_of_quoted_mod_hash(&programs::SINGLETON_TOP_LAYER_V1_1_TREE_HASH),
            &[self.singleton().tree_hash(), self.inner_puzzle_hash(owner)],
        )
    }

    pub fn puzzle(&self, inner: &Program<'_>) -> Program<'static> {
        let recovery = self
            .recovery_list_hash
            .map_or_else(|| Program::to(0), Program::to);
        let did = programs::DID_INNERPUZ_PROGRAM.curry(&[
            inner.to_owned(),
            recovery,
            Program::to(self.verifications_required),
            self.singleton(),
            self.metadata.clone(),
        ]);
        programs::SINGLETON_TOP_LAYER_V1_1_PROGRAM
            .curry(&[self.singleton(), did])
            .to_owned()
    }

    pub fn spend(
        &self,
        coin: Coin,
        lineage: Program<'static>,
        inner: &Program<'_>,
        solution: &Program<'_>,
    ) -> Result<CoinSpend, Error> {
        let puzzle = self.puzzle(inner);
        if puzzle.tree_hash() != coin.puzzle_hash {
            return Err(Error::other("DID puzzle does not match coin"));
        }
        let did_solution = SExp::from(vec![SExp::from(1), solution.sexp().to_owned()]);
        Ok(CoinSpend {
            coin,
            puzzle_reveal: puzzle.serialized()?,
            solution: Program::to(vec![
                lineage.sexp().to_owned(),
                coin.amount.into(),
                did_solution,
            ])
            .serialized()?,
        })
    }
}

#[derive(Clone)]
pub struct DidCoin {
    pub coin: Coin,
    pub info: DidInfo,
    pub owner_puzzle_hash: Bytes32,
    pub lineage_proof: Program<'static>,
}

impl DidCoin {
    pub fn parse_child(
        coin: Coin,
        parent: &CoinSpend,
        owners: &std::collections::HashSet<Bytes32>,
        max_cost: u64,
    ) -> Result<Option<Self>, Error> {
        let puzzle = parent.puzzle_reveal.to_program()?;
        let (module, args) = puzzle.uncurry()?;
        if module.tree_hash() != programs::SINGLETON_TOP_LAYER_V1_1_TREE_HASH {
            return Ok(None);
        }
        if !args.sexp().arg_count_is(2) {
            return Err(Error::other("invalid singleton arguments"));
        }
        let singleton_args = args.sexp().ref_list();
        let inner = Program::new(singleton_args[1].to_owned());
        let (module, args) = inner.uncurry()?;
        if module.tree_hash() != programs::DID_INNERPUZ_TREE_HASH {
            return Ok(None);
        }
        if !args.sexp().arg_count_is(5) {
            return Err(Error::other("invalid DID arguments"));
        }
        let fields = args.sexp().ref_list();
        let info = DidInfo {
            launcher_id: Bytes32::try_from(singleton_args[0].rest()?.first()?)?,
            recovery_list_hash: if fields[1].atom()?.as_ref().is_empty() {
                None
            } else {
                Some(Bytes32::try_from(fields[1])?)
            },
            verifications_required: fields[2]
                .as_int()?
                .to_u64()
                .ok_or_else(|| Error::other("invalid DID recovery threshold"))?,
            metadata: Program::new(fields[4].to_owned()),
        };
        if fields[3] != info.singleton().sexp() || singleton_args[0] != info.singleton().sexp() {
            return Err(Error::other("DID singleton mismatch"));
        }
        if parent.coin.name() != coin.parent_coin_info
            || puzzle.tree_hash() != parent.coin.puzzle_hash
        {
            return Err(Error::other("DID parent mismatch"));
        }
        if !parent
            .compute_additions_with_cost(max_cost)?
            .0
            .contains(&coin)
        {
            return Err(Error::other("DID is not an output of its parent"));
        }
        for owner in owners {
            if info.puzzle_hash(*owner) == coin.puzzle_hash {
                return Ok(Some(Self {
                    coin,
                    info,
                    owner_puzzle_hash: *owner,
                    lineage_proof: Program::to(vec![
                        SExp::from(parent.coin.parent_coin_info),
                        inner.tree_hash().into(),
                        parent.coin.amount.into(),
                    ]),
                }));
            }
        }
        Ok(None)
    }
}

pub fn launch_did(
    parent: Bytes32,
    inner: &Program<'_>,
) -> Result<(Vec<SExp<'static>>, Vec<CoinSpend>), Error> {
    let launcher = Coin {
        parent_coin_info: parent,
        puzzle_hash: programs::SINGLETON_LAUNCHER_TREE_HASH,
        amount: 1,
    };
    let info = DidInfo {
        launcher_id: launcher.name(),
        recovery_list_hash: Some(Program::to(0).tree_hash()),
        verifications_required: 0,
        metadata: Program::to(0),
    };
    let eve = Coin {
        parent_coin_info: launcher.name(),
        puzzle_hash: info.puzzle(inner).tree_hash(),
        amount: 1,
    };
    let launcher_solution = Program::to(vec![
        SExp::from(eve.puzzle_hash),
        SExp::from(1),
        SExp::from(0),
    ]);
    let mut announcement = launcher.name().bytes().to_vec();
    announcement.extend_from_slice(launcher_solution.tree_hash().as_ref());
    let conditions = vec![
        SExp::from(vec![
            SExp::from(51),
            launcher.puzzle_hash.into(),
            SExp::from(1),
        ]),
        SExp::from(vec![
            SExp::from(61),
            SExp::from(dg_xch_core::utils::hash_256(announcement).to_vec()),
        ]),
    ];
    let outputs = vec![SExp::from(vec![
        SExp::from(51),
        info.inner_puzzle_hash(inner.tree_hash()).into(),
        SExp::from(1),
        SExp::from(vec![SExp::from(inner.tree_hash())]),
    ])];
    let solution = crate::p2_delegated_puzzle_or_hidden_puzzle::solution_for_conditions(outputs)?;
    Ok((
        conditions,
        vec![
            CoinSpend {
                coin: launcher,
                puzzle_reveal: programs::SINGLETON_LAUNCHER_PROGRAM.serialized()?,
                solution: launcher_solution.serialized()?,
            },
            info.spend(
                eve,
                Program::to(vec![SExp::from(parent), SExp::from(1)]),
                inner,
                &solution,
            )?,
        ],
    ))
}
