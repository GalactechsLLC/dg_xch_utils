use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_core::consensus::coinbase::pool_parent_id;
use std::io::Error;

#[derive(Clone)]
pub struct PoolConfig {
    pub target: Bytes32,
    pub relative_lock_height: u32,
    pub memoization: SerializedProgram,
}

#[derive(Clone)]
pub struct PlotNft {
    pub launcher_id: Bytes32,
    pub genesis_challenge: Bytes32,
    pub synthetic_public_key: Bytes48,
    pub pool: Option<PoolConfig>,
    pub exiting: bool,
}

fn program(bytes: &[u8]) -> Result<Program<'static>, Error> {
    Ok(SerializedProgram::from_bytes(bytes)
        .to_program()?
        .to_owned())
}

fn embedded(hex: &str) -> Result<Program<'static>, Error> {
    program(&hex::decode(hex.trim()).map_err(Error::other)?)
}

fn list(items: Vec<Program<'static>>) -> Program<'static> {
    Program::to(
        items
            .into_iter()
            .map(|item| item.sexp().to_owned())
            .collect::<Vec<SExp<'static>>>(),
    )
}

fn quoted(value: &Program<'_>) -> Program<'static> {
    Program::new(SExp::from(1u8).cons(value.sexp().to_owned()))
}

fn mips(
    member: Program<'static>,
    restrictions: Vec<Program<'static>>,
    top: bool,
) -> Result<Program<'static>, Error> {
    let mut inner = member;
    if !restrictions.is_empty() {
        inner = program(&chia_puzzles::RESTRICTIONS)?
            .curry(&[list(Vec::new()), list(restrictions), inner])
            .to_owned();
    }
    if top {
        inner = program(&chia_puzzles::DELEGATED_PUZZLE_FEEDER)?
            .curry(&[inner])
            .to_owned();
    }
    Ok(
        list(vec![Program::to(2u8), Program::to(5u8), Program::to(7u8)])
            .curry(&[Program::to(0u8), inner])
            .to_owned(),
    )
}

pub fn singleton_struct(launcher: Bytes32) -> Program<'static> {
    Program::to((
        Bytes32::from(chia_puzzles::SINGLETON_TOP_LAYER_V1_1_HASH),
        (
            launcher,
            Bytes32::from(chia_puzzles::SINGLETON_LAUNCHER_HASH),
        ),
    ))
}

pub fn reward_puzzle(launcher: Bytes32) -> Result<Program<'static>, Error> {
    mips(
        program(&chia_puzzles::SINGLETON_MEMBER)?
            .curry(&[singleton_struct(launcher)])
            .to_owned(),
        Vec::new(),
        true,
    )
}

impl PlotNft {
    pub fn claim_reward(
        &self,
        singleton: Coin,
        parent: &CoinSpend,
        reward: Coin,
        height: u32,
    ) -> Result<Vec<CoinSpend>, Error> {
        let full_puzzle = self.puzzle()?;
        if self.pool.is_none()
            || singleton.amount != 1
            || singleton.puzzle_hash != full_puzzle.tree_hash()
            || singleton.parent_coin_info != parent.coin.name()
            || reward.parent_coin_info != pool_parent_id(height, self.genesis_challenge)
            || reward.puzzle_hash != reward_puzzle(self.launcher_id)?.tree_hash()
            || reward.amount == 0
        {
            return Err(Error::other("invalid pooling-v2 reward or singleton"));
        }
        let parent_puzzle = parent.puzzle_reveal.to_program()?;
        let (module, arguments) = parent_puzzle.uncurry()?;
        let arguments = arguments.as_list();
        if parent_puzzle.tree_hash() != parent.coin.puzzle_hash
            || module.tree_hash() != Bytes32::from(chia_puzzles::SINGLETON_TOP_LAYER_V1_1_HASH)
            || arguments.len() != 2
            || arguments.first().map(Program::tree_hash)
                != Some(singleton_struct(self.launcher_id).tree_hash())
            || !parent
                .compute_additions_with_cost(500_000_000)?
                .0
                .contains(&singleton)
        {
            return Err(Error::other("invalid pooling-v2 singleton lineage"));
        }
        let parent_inner = arguments
            .get(1)
            .ok_or_else(|| Error::other("missing singleton inner puzzle"))?;
        let inner_hash = self.inner_puzzle()?.tree_hash();
        let lineage = list(vec![
            parent.coin.parent_coin_info.into(),
            parent_inner.tree_hash().into(),
            Program::to(parent.coin.amount),
        ]);
        let sibling = Program::to(self.user_branch()?.tree_hash()).tree_hash();
        let proof = list(vec![Program::to(1u8), sibling.into()]);
        let inner_solution = list(vec![
            self.claim_puzzle()?,
            list(vec![
                inner_hash.into(),
                Program::to(height),
                Program::to(reward.amount),
            ]),
            proof,
            self.pool_branch()?,
            list(Vec::new()),
        ]);
        let solution = list(vec![lineage, Program::to(singleton.amount), inner_solution]);
        let reward_solution = list(vec![
            self.forward_puzzle()?,
            list(vec![Program::to(reward.amount)]),
            inner_hash.into(),
        ]);
        Ok(vec![
            CoinSpend {
                coin: singleton,
                puzzle_reveal: full_puzzle.serialized()?,
                solution: solution.serialized()?,
            },
            CoinSpend {
                coin: reward,
                puzzle_reveal: reward_puzzle(self.launcher_id)?.serialized()?,
                solution: reward_solution.serialized()?,
            },
        ])
    }

    pub fn forward_puzzle(&self) -> Result<Program<'static>, Error> {
        let pool = self
            .pool
            .as_ref()
            .ok_or_else(|| Error::other("PlotNFT is not pooling"))?;
        Ok(embedded(include_str!(
            "pool_v2/forward_to_pool_puzzle_hash_dpuz.clsp.hex"
        ))?
        .curry(&[
            pool.target.into(),
            pool.memoization.to_program()?.to_owned(),
        ])
        .to_owned())
    }

    pub fn claim_puzzle(&self) -> Result<Program<'static>, Error> {
        let genesis: &[u8] = self.genesis_challenge.as_ref();
        Ok(
            embedded(include_str!("pool_v2/claim_pool_rewards_dpuz.clsp.hex"))?
                .curry(&[
                    Program::to(genesis[..16].to_vec()),
                    Bytes32::from(chia_puzzles::SINGLETON_TOP_LAYER_V1_1_HASH).into(),
                    singleton_struct(self.launcher_id).tree_hash().into(),
                    reward_puzzle(self.launcher_id)?.tree_hash().into(),
                    self.forward_puzzle()?.tree_hash().into(),
                ])
                .to_owned(),
        )
    }

    fn user_member(&self) -> Result<Program<'static>, Error> {
        program(&chia_puzzles::BLS_WITH_TAPROOT_MEMBER)
            .map(|puzzle| puzzle.curry(&[self.synthetic_public_key.into()]).to_owned())
    }

    fn user_restriction(&self) -> Result<Program<'static>, Error> {
        let pool = self
            .pool
            .as_ref()
            .ok_or_else(|| Error::other("PlotNFT is not pooling"))?;
        let first = if self.exiting {
            embedded(include_str!("pool_v2/heightlock.clsp.hex"))?
                .curry(&[Program::to(pool.relative_lock_height)])
                .to_owned()
        } else {
            let mut waiting_room = self.clone();
            waiting_room.exiting = true;
            embedded(include_str!(
                "pool_v2/fixed_create_coin_destinations.clsp.hex"
            ))?
            .curry(&[waiting_room.inner_puzzle()?.tree_hash().into()])
            .to_owned()
        };
        let banned = embedded(include_str!("pool_v2/send_message_banned.clsp.hex"))?;
        Ok(program(&chia_puzzles::ENFORCE_DPUZ_WRAPPERS)?
            .curry(&[
                quoted(&program(&chia_puzzles::ADD_DPUZ_WRAPPER)?)
                    .tree_hash()
                    .into(),
                list(vec![
                    quoted(&first).tree_hash().into(),
                    quoted(&banned).tree_hash().into(),
                ]),
            ])
            .to_owned())
    }

    fn user_branch(&self) -> Result<Program<'static>, Error> {
        mips(self.user_member()?, vec![self.user_restriction()?], false)
    }

    fn pool_branch(&self) -> Result<Program<'static>, Error> {
        mips(
            program(&chia_puzzles::FIXED_PUZZLE_MEMBER)?
                .curry(&[self.claim_puzzle()?.tree_hash().into()])
                .to_owned(),
            Vec::new(),
            false,
        )
    }

    pub fn inner_puzzle(&self) -> Result<Program<'static>, Error> {
        if self.pool.is_none() {
            if self.exiting {
                return Err(Error::other("self-pooling PlotNFT cannot be exiting"));
            }
            return mips(self.user_member()?, Vec::new(), true);
        }
        let left = self.user_branch()?.tree_hash();
        let right = self.pool_branch()?.tree_hash();
        let root = Program::to((left, right)).tree_hash();
        mips(
            program(&chia_puzzles::ONE_OF_N)?
                .curry(&[root.into()])
                .to_owned(),
            Vec::new(),
            true,
        )
    }

    pub fn puzzle(&self) -> Result<Program<'static>, Error> {
        Ok(program(&chia_puzzles::SINGLETON_TOP_LAYER_V1_1)?
            .curry(&[singleton_struct(self.launcher_id), self.inner_puzzle()?])
            .to_owned())
    }

    pub fn memo(&self) -> Result<Program<'static>, Error> {
        fn memo(items: Vec<Program<'static>>) -> Program<'static> {
            Program::new(SExp::from("CHIP-0043").cons(list(items).sexp().to_owned()))
        }
        let mut additional = vec![self.synthetic_public_key.into()];
        let hint = if let Some(pool) = &self.pool {
            additional.extend([
                pool.target.into(),
                Program::to(pool.relative_lock_height),
                pool.memoization.to_program()?.to_owned(),
            ]);
            let restrictions = list(vec![list(vec![
                Program::to(0u8),
                self.user_restriction()?.tree_hash().into(),
                list(vec![Program::to(0u8), Program::to(0u8)]),
            ])]);
            let user = memo(vec![
                Program::to(0u8),
                restrictions,
                Program::to(0u8),
                list(vec![
                    self.user_member()?.tree_hash().into(),
                    Program::to(0u8),
                ]),
            ]);
            let member = program(&chia_puzzles::FIXED_PUZZLE_MEMBER)?
                .curry(&[self.claim_puzzle()?.tree_hash().into()])
                .tree_hash();
            let pool = memo(vec![
                Program::to(0u8),
                list(Vec::new()),
                Program::to(0u8),
                list(vec![member.into(), Program::to(0u8)]),
            ]);
            list(vec![Program::to(1u8), list(vec![user, pool])])
        } else {
            list(vec![
                self.user_member()?.tree_hash().into(),
                Program::to(0u8),
            ])
        };
        Ok(memo(vec![
            Program::to(0u8),
            list(Vec::new()),
            Program::to(u8::from(self.pool.is_some())),
            hint,
            list(additional),
        ]))
    }

    pub fn launch(
        origin: Coin,
        genesis_challenge: Bytes32,
        synthetic_public_key: Bytes48,
        pool: Option<PoolConfig>,
        hint: Bytes32,
    ) -> Result<crate::pool_launch::PoolLaunch, Error> {
        if origin.amount == 0 {
            return Err(Error::other("PlotNFT launch needs a funding coin"));
        }
        blst::min_pk::PublicKey::key_validate(synthetic_public_key.as_ref())
            .map_err(|_| Error::other("invalid PlotNFT public key"))?;
        if pool.as_ref().is_some_and(|pool| {
            pool.relative_lock_height == 0
                || pool.relative_lock_height > 1000
                || pool.memoization.to_bytes().len() > 4096
        }) {
            return Err(Error::other(
                "invalid pooling-v2 lock height or memoization",
            ));
        }
        let launcher = Coin {
            parent_coin_info: origin.name(),
            puzzle_hash: chia_puzzles::SINGLETON_LAUNCHER_HASH.into(),
            amount: 1,
        };
        let definition = Self {
            launcher_id: launcher.name(),
            genesis_challenge,
            synthetic_public_key,
            pool,
            exiting: false,
        };
        let memo = Program::new(SExp::from(hint).cons(definition.memo()?.sexp().to_owned()));
        let inner = quoted(&list(vec![
            list(vec![
                Program::to(51u8),
                definition.inner_puzzle()?.tree_hash().into(),
                Program::to(1u8),
                memo,
            ]),
            list(vec![Program::to(60u8), Program::to(0u8)]),
        ]));
        let revision = program(&chia_puzzles::SINGLETON_TOP_LAYER_V1_1)?
            .curry(&[singleton_struct(launcher.name()), inner])
            .to_owned();
        let revision_coin = Coin {
            parent_coin_info: launcher.name(),
            puzzle_hash: revision.tree_hash(),
            amount: 1,
        };
        let launcher_solution = list(vec![
            revision.tree_hash().into(),
            Program::to(1u8),
            Program::to(0u8),
        ]);
        let revision_solution = list(vec![
            list(vec![launcher.parent_coin_info.into(), Program::to(1u8)]),
            Program::to(1u8),
            Program::to(0u8),
        ]);
        let announcement: Bytes32 = dg_xch_core::utils::hash_256(
            [
                &launcher.name()[0..32],
                &launcher_solution.tree_hash()[0..32],
            ]
            .concat(),
        )
        .into();
        let revision_announcement: Bytes32 =
            dg_xch_core::utils::hash_256(revision_coin.name()).into();
        Ok(crate::pool_launch::PoolLaunch {
            launcher_id: launcher.name(),
            contract_puzzle_hash: reward_puzzle(launcher.name())?.tree_hash(),
            singleton: Coin {
                parent_coin_info: revision_coin.name(),
                puzzle_hash: definition.puzzle()?.tree_hash(),
                amount: 1,
            },
            spends: vec![
                CoinSpend {
                    coin: launcher,
                    puzzle_reveal: program(&chia_puzzles::SINGLETON_LAUNCHER)?.serialized()?,
                    solution: launcher_solution.serialized()?,
                },
                CoinSpend {
                    coin: revision_coin,
                    puzzle_reveal: revision.serialized()?,
                    solution: revision_solution.serialized()?,
                },
            ],
            funding_conditions: list(vec![
                list(vec![
                    Program::to(51u8),
                    launcher.puzzle_hash.into(),
                    Program::to(1u8),
                ]),
                list(vec![Program::to(61u8), announcement.into()]),
                list(vec![Program::to(61u8), revision_announcement.into()]),
            ]),
        })
    }
}
