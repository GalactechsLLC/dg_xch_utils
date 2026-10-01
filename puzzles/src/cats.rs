use crate::lineage_proof::LineageProof;
use dg_parser_macro::parse_program_hex;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::program::Program;
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_core::clvm::utils::INFINITE_COST;
use std::io::{Error, ErrorKind};

parse_program_hex!(
    CAT_1,
    "ff02ffff01ff02ff5effff04ff02ffff04ffff04ff05ffff04ffff0bff2cff0580ffff04ff0bff80808080ffff04ffff02ff17ff2f80ffff04ff5fffff04ffff02ff2effff04ff02ffff04ff17ff80808080ffff04ffff0bff82027fff82057fff820b7f80ffff04ff81bfffff04ff82017fffff04ff8202ffffff04ff8205ffffff04ff820bffff80808080808080808080808080ffff04ffff01ffffffff81ca3dff46ff0233ffff3c04ff01ff0181cbffffff02ff02ffff03ff05ffff01ff02ff32ffff04ff02ffff04ff0dffff04ffff0bff22ffff0bff2cff3480ffff0bff22ffff0bff22ffff0bff2cff5c80ff0980ffff0bff22ff0bffff0bff2cff8080808080ff8080808080ffff010b80ff0180ffff02ffff03ff0bffff01ff02ffff03ffff09ffff02ff2effff04ff02ffff04ff13ff80808080ff820b9f80ffff01ff02ff26ffff04ff02ffff04ffff02ff13ffff04ff5fffff04ff17ffff04ff2fffff04ff81bfffff04ff82017fffff04ff1bff8080808080808080ffff04ff82017fff8080808080ffff01ff088080ff0180ffff01ff02ffff03ff17ffff01ff02ffff03ffff20ff81bf80ffff0182017fffff01ff088080ff0180ffff01ff088080ff018080ff0180ffff04ffff04ff05ff2780ffff04ffff10ff0bff5780ff778080ff02ffff03ff05ffff01ff02ffff03ffff09ffff02ffff03ffff09ff11ff7880ffff0159ff8080ff0180ffff01818f80ffff01ff02ff7affff04ff02ffff04ff0dffff04ff0bffff04ffff04ff81b9ff82017980ff808080808080ffff01ff02ff5affff04ff02ffff04ffff02ffff03ffff09ff11ff7880ffff01ff04ff78ffff04ffff02ff36ffff04ff02ffff04ff13ffff04ff29ffff04ffff0bff2cff5b80ffff04ff2bff80808080808080ff398080ffff01ff02ffff03ffff09ff11ff2480ffff01ff04ff24ffff04ffff0bff20ff2980ff398080ffff010980ff018080ff0180ffff04ffff02ffff03ffff09ff11ff7880ffff0159ff8080ff0180ffff04ffff02ff7affff04ff02ffff04ff0dffff04ff0bffff04ff17ff808080808080ff80808080808080ff0180ffff01ff04ff80ffff04ff80ff17808080ff0180ffffff02ffff03ff05ffff01ff04ff09ffff02ff26ffff04ff02ffff04ff0dffff04ff0bff808080808080ffff010b80ff0180ff0bff22ffff0bff2cff5880ffff0bff22ffff0bff22ffff0bff2cff5c80ff0580ffff0bff22ffff02ff32ffff04ff02ffff04ff07ffff04ffff0bff2cff2c80ff8080808080ffff0bff2cff8080808080ffff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff2effff04ff02ffff04ff09ff80808080ffff02ff2effff04ff02ffff04ff0dff8080808080ffff01ff0bff2cff058080ff0180ffff04ffff04ff28ffff04ff5fff808080ffff02ff7effff04ff02ffff04ffff04ffff04ff2fff0580ffff04ff5fff82017f8080ffff04ffff02ff7affff04ff02ffff04ff0bffff04ff05ffff01ff808080808080ffff04ff17ffff04ff81bfffff04ff82017fffff04ffff0bff8204ffffff02ff36ffff04ff02ffff04ff09ffff04ff820affffff04ffff0bff2cff2d80ffff04ff15ff80808080808080ff8216ff80ffff04ff8205ffffff04ff820bffff808080808080808080808080ff02ff2affff04ff02ffff04ff5fffff04ff3bffff04ffff02ffff03ff17ffff01ff09ff2dffff0bff27ffff02ff36ffff04ff02ffff04ff29ffff04ff57ffff04ffff0bff2cff81b980ffff04ff59ff80808080808080ff81b78080ff8080ff0180ffff04ff17ffff04ff05ffff04ff8202ffffff04ffff04ffff04ff24ffff04ffff0bff7cff2fff82017f80ff808080ffff04ffff04ff30ffff04ffff0bff81bfffff0bff7cff15ffff10ff82017fffff11ff8202dfff2b80ff8202ff808080ff808080ff138080ff80808080808080808080ff018080"
);
parse_program_hex!(
    CAT_2,
    "ff02ffff01ff02ff5effff04ff02ffff04ffff04ff05ffff04ffff0bff34ff0580ffff04ff0bff80808080ffff04ffff02ff17ff2f80ffff04ff5fffff04ffff02ff2effff04ff02ffff04ff17ff80808080ffff04ffff02ff2affff04ff02ffff04ff82027fffff04ff82057fffff04ff820b7fff808080808080ffff04ff81bfffff04ff82017fffff04ff8202ffffff04ff8205ffffff04ff820bffff80808080808080808080808080ffff04ffff01ffffffff3d46ff02ff333cffff0401ff01ff81cb02ffffff20ff02ffff03ff05ffff01ff02ff32ffff04ff02ffff04ff0dffff04ffff0bff7cffff0bff34ff2480ffff0bff7cffff0bff7cffff0bff34ff2c80ff0980ffff0bff7cff0bffff0bff34ff8080808080ff8080808080ffff010b80ff0180ffff02ffff03ffff22ffff09ffff0dff0580ff2280ffff09ffff0dff0b80ff2280ffff15ff17ffff0181ff8080ffff01ff0bff05ff0bff1780ffff01ff088080ff0180ffff02ffff03ff0bffff01ff02ffff03ffff09ffff02ff2effff04ff02ffff04ff13ff80808080ff820b9f80ffff01ff02ff56ffff04ff02ffff04ffff02ff13ffff04ff5fffff04ff17ffff04ff2fffff04ff81bfffff04ff82017fffff04ff1bff8080808080808080ffff04ff82017fff8080808080ffff01ff088080ff0180ffff01ff02ffff03ff17ffff01ff02ffff03ffff20ff81bf80ffff0182017fffff01ff088080ff0180ffff01ff088080ff018080ff0180ff04ffff04ff05ff2780ffff04ffff10ff0bff5780ff778080ffffff02ffff03ff05ffff01ff02ffff03ffff09ffff02ffff03ffff09ff11ff5880ffff0159ff8080ff0180ffff01818f80ffff01ff02ff26ffff04ff02ffff04ff0dffff04ff0bffff04ffff04ff81b9ff82017980ff808080808080ffff01ff02ff7affff04ff02ffff04ffff02ffff03ffff09ff11ff5880ffff01ff04ff58ffff04ffff02ff76ffff04ff02ffff04ff13ffff04ff29ffff04ffff0bff34ff5b80ffff04ff2bff80808080808080ff398080ffff01ff02ffff03ffff09ff11ff7880ffff01ff02ffff03ffff20ffff02ffff03ffff09ffff0121ffff0dff298080ffff01ff02ffff03ffff09ffff0cff29ff80ff3480ff5c80ffff01ff0101ff8080ff0180ff8080ff018080ffff0109ffff01ff088080ff0180ffff010980ff018080ff0180ffff04ffff02ffff03ffff09ff11ff5880ffff0159ff8080ff0180ffff04ffff02ff26ffff04ff02ffff04ff0dffff04ff0bffff04ff17ff808080808080ff80808080808080ff0180ffff01ff04ff80ffff04ff80ff17808080ff0180ffff02ffff03ff05ffff01ff04ff09ffff02ff56ffff04ff02ffff04ff0dffff04ff0bff808080808080ffff010b80ff0180ff0bff7cffff0bff34ff2880ffff0bff7cffff0bff7cffff0bff34ff2c80ff0580ffff0bff7cffff02ff32ffff04ff02ffff04ff07ffff04ffff0bff34ff3480ff8080808080ffff0bff34ff8080808080ffff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff2effff04ff02ffff04ff09ff80808080ffff02ff2effff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff058080ff0180ffff04ffff04ff30ffff04ff5fff808080ffff02ff7effff04ff02ffff04ffff04ffff04ff2fff0580ffff04ff5fff82017f8080ffff04ffff02ff26ffff04ff02ffff04ff0bffff04ff05ffff01ff808080808080ffff04ff17ffff04ff81bfffff04ff82017fffff04ffff02ff2affff04ff02ffff04ff8204ffffff04ffff02ff76ffff04ff02ffff04ff09ffff04ff820affffff04ffff0bff34ff2d80ffff04ff15ff80808080808080ffff04ff8216ffff808080808080ffff04ff8205ffffff04ff820bffff808080808080808080808080ff02ff5affff04ff02ffff04ff5fffff04ff3bffff04ffff02ffff03ff17ffff01ff09ff2dffff02ff2affff04ff02ffff04ff27ffff04ffff02ff76ffff04ff02ffff04ff29ffff04ff57ffff04ffff0bff34ff81b980ffff04ff59ff80808080808080ffff04ff81b7ff80808080808080ff8080ff0180ffff04ff17ffff04ff05ffff04ff8202ffffff04ffff04ffff04ff78ffff04ffff0eff5cffff02ff2effff04ff02ffff04ffff04ff2fffff04ff82017fff808080ff8080808080ff808080ffff04ffff04ff20ffff04ffff0bff81bfff5cffff02ff2effff04ff02ffff04ffff04ff15ffff04ffff10ff82017fffff11ff8202dfff2b80ff8202ff80ff808080ff8080808080ff808080ff138080ff80808080808080808080ff018080"
);

#[test]
pub fn test_hashes() {
    assert_eq!(
        Bytes32::const_hex("72dec062874cd4d3aab892a0906688a1ae412b0109982e1797a170add88bdcdc"),
        CAT_1_TREE_HASH
    );
    assert_eq!(
        Bytes32::const_hex("37bef360ee858133b69d595a906dc45d01af50379dad515eb9518abb7c1d2a7a"),
        CAT_2_TREE_HASH
    );
}

pub enum Cat<'a> {
    V1(CatPuzzleCurriedArgs<'a>, CatSolution<'a>),
    V2(CatPuzzleCurriedArgs<'a>, CatSolution<'a>),
}

#[derive(Debug, PartialEq)]
pub struct CatPuzzleCurriedArgs<'a> {
    pub mod_hash: Bytes32,
    pub tail_program_hash: Bytes32,
    pub inner_puzzle: Program<'a>,
}
impl<'a> From<&CatPuzzleCurriedArgs<'a>> for SExp<'a> {
    fn from(args: &CatPuzzleCurriedArgs<'a>) -> SExp<'a> {
        vec![
            args.mod_hash.into(),
            args.tail_program_hash.into(),
            args.inner_puzzle.sexp().to_owned(),
        ]
        .into()
    }
}
impl<'a> TryFrom<&SExp<'a>> for CatPuzzleCurriedArgs<'a> {
    type Error = Error;
    fn try_from(sexp: &SExp<'a>) -> Result<Self, Self::Error> {
        let (mod_hash, rest) = sexp.split()?;
        let (tail_program_hash, rest) = rest.split()?;
        let (inner_puzzle, _) = rest.split()?;
        Ok(Self {
            mod_hash: Bytes32::try_from(mod_hash)?,
            tail_program_hash: Bytes32::try_from(tail_program_hash)?,
            inner_puzzle: Program::new(inner_puzzle.to_owned()),
        })
    }
}
impl<'a> Cat<'a> {
    pub fn new(curried_args: CatPuzzleCurriedArgs<'a>, solution: CatSolution<'a>) -> Cat<'a> {
        Cat::V2(curried_args, solution)
    }
    pub fn new_v1(curried_args: CatPuzzleCurriedArgs<'a>, solution: CatSolution<'a>) -> Cat<'a> {
        Cat::V1(curried_args, solution)
    }
    pub fn tree_hash(&self) -> Bytes32 {
        match self {
            Cat::V1(_, _) => CAT_1_PROGRAM.tree_hash(),
            Cat::V2(_, _) => CAT_2_PROGRAM.tree_hash(),
        }
    }
    pub fn curried_tree_hash(&self) -> Result<Bytes32, Error> {
        let args = self.curried_args();
        Ok(self
            .puzzle_reveal()
            .curry(&[
                Program::new(args.mod_hash.into()),
                Program::new(args.tail_program_hash.into()),
                args.inner_puzzle.to_owned(),
            ])
            .tree_hash())
    }
    pub fn curried_args(&'a self) -> &'a CatPuzzleCurriedArgs<'a> {
        match self {
            Cat::V1(curried_args, _) => curried_args,
            Cat::V2(curried_args, _) => curried_args,
        }
    }
    pub fn curried_args_mut(&'a mut self) -> &'a mut CatPuzzleCurriedArgs<'a> {
        match self {
            Cat::V1(curried_args, _) => curried_args,
            Cat::V2(curried_args, _) => curried_args,
        }
    }
    pub fn puzzle_reveal(&self) -> &Program<'static> {
        match self {
            Cat::V1(_, _) => &CAT_1_PROGRAM,
            Cat::V2(_, _) => &CAT_2_PROGRAM,
        }
    }
    pub fn run(&'a self, solution: CatSolution<'a>) -> Result<Program<'static>, Error> {
        let curried_args = self.curried_args();
        let args = Program::new(
            SExp::from(curried_args.mod_hash).cons(
                SExp::from(curried_args.tail_program_hash).cons(
                    curried_args
                        .inner_puzzle
                        .sexp()
                        .to_owned()
                        .cons(solution.into()),
                ),
            ),
        );
        let (_cost, results) = self.puzzle_reveal().run(INFINITE_COST, 0, &args)?;
        Ok(results.to_owned())
    }
}

pub struct CatSolution<'a> {
    inner_puzzle_solution: Program<'a>,
    lineage_proof: LineageProof,
    prev_coin_id: Bytes32,
    this_coin_info: Coin,
    next_coin_proof: NextCoinProof,
    prev_subtotal: u64,
    extra_delta: Program<'a>,
}
impl<'a> From<CatSolution<'a>> for SExp<'a> {
    fn from(solution: CatSolution<'a>) -> SExp<'a> {
        vec![
            solution.inner_puzzle_solution.sexp().to_owned(),
            solution.lineage_proof.into(),
            solution.prev_coin_id.into(),
            solution.this_coin_info.into(),
            solution.next_coin_proof.into(),
            solution.prev_subtotal.into(),
            solution.extra_delta.sexp().to_owned(),
        ]
        .into()
    }
}
impl<'a> TryFrom<&SExp<'a>> for CatSolution<'a> {
    type Error = Error;
    fn try_from(sexp: &SExp<'a>) -> Result<Self, Self::Error> {
        let (inner_puzzle_solution, rest) = sexp.split()?;
        let (lineage_proof, rest) = rest.split()?;
        let (prev_coin_id, rest) = rest.split()?;
        let (this_coin_info, rest) = rest.split()?;
        let (next_coin_proof, rest) = rest.split()?;
        let (prev_subtotal, rest) = rest.split()?;
        let (extra_delta, _) = rest.split()?;
        Ok(Self {
            inner_puzzle_solution: Program::new(inner_puzzle_solution.to_owned()),
            lineage_proof: LineageProof::try_from(lineage_proof)?,
            prev_coin_id: Bytes32::try_from(prev_coin_id)?,
            this_coin_info: Coin::try_from(this_coin_info)?,
            next_coin_proof: NextCoinProof::try_from(next_coin_proof)?,
            prev_subtotal: prev_subtotal
                .as_int()?
                .to_u64()
                .ok_or(Error::new(ErrorKind::InvalidData, "Invalid prev_subtotal"))?,
            extra_delta: Program::new(extra_delta.to_owned()),
        })
    }
}

pub struct NextCoinProof {
    pub parent_coin_info: Bytes32,
    pub inner_puzzle_hash: Bytes32,
    pub amount: u64,
}
impl<'a> From<NextCoinProof> for SExp<'a> {
    fn from(coin: NextCoinProof) -> SExp<'a> {
        (&coin).into()
    }
}
impl<'a> From<&NextCoinProof> for SExp<'a> {
    fn from(input: &NextCoinProof) -> SExp<'a> {
        (&[
            SExp::from(input.parent_coin_info),
            SExp::from(input.inner_puzzle_hash),
            SExp::from(input.amount),
        ])
            .into()
    }
}
impl TryFrom<&SExp<'_>> for NextCoinProof {
    type Error = Error;
    fn try_from(sexp: &SExp) -> Result<Self, Self::Error> {
        let (parent_coin_info, rest) = sexp.split()?;
        let (inner_puzzle_hash, rest) = rest.split()?;
        let (amount, _) = rest.split()?;
        Ok(Self {
            parent_coin_info: Bytes32::try_from(parent_coin_info)?,
            inner_puzzle_hash: Bytes32::try_from(inner_puzzle_hash)?,
            amount: amount
                .as_int()?
                .to_u64()
                .ok_or(Error::new(ErrorKind::InvalidData, "Invalid prev_subtotal"))?,
        })
    }
}

#[test]
fn test_cat_currying_and_run() {
    for (module, mod_hash, expected_hash) in [
        (
            &CAT_1_PROGRAM,
            CAT_1_TREE_HASH,
            "9e18a400acfd3b2662be43e4bbf288aa837a5486981e8913a1b31f87ca4e5cf9",
        ),
        (
            &CAT_2_PROGRAM,
            CAT_2_TREE_HASH,
            "9c05187421ca70664cb10a27e355b930a42f063aad534e5a42b0fb6adea8e375",
        ),
    ] {
        let args = CatPuzzleCurriedArgs {
            mod_hash,
            tail_program_hash: [15; 32].into(),
            inner_puzzle: Program::to(1),
        };
        let puzzle = module.curry(&[
            Program::new(args.mod_hash.into()),
            Program::new(args.tail_program_hash.into()),
            args.inner_puzzle.clone(),
        ]);
        let parent = Coin {
            parent_coin_info: [27; 32].into(),
            puzzle_hash: puzzle.tree_hash(),
            amount: 100,
        };
        let coin = Coin {
            parent_coin_info: parent.name(),
            ..parent
        };
        let inner_puzzle_hash = args.inner_puzzle.tree_hash();
        let solution = SExp::from(CatSolution {
            inner_puzzle_solution: Program::to(&[SExp::from(vec![
                SExp::from(51),
                inner_puzzle_hash.into(),
                SExp::from(100u64),
            ])]),
            lineage_proof: LineageProof {
                parent_parent_id: parent.parent_coin_info,
                parent_inner_puzzle_hash: inner_puzzle_hash,
                parent_amount: parent.amount,
            },
            prev_coin_id: coin.name(),
            this_coin_info: coin,
            next_coin_proof: NextCoinProof {
                parent_coin_info: coin.parent_coin_info,
                inner_puzzle_hash,
                amount: coin.amount,
            },
            prev_subtotal: 0,
            extra_delta: Program::to(0),
        });
        let cat = if mod_hash == CAT_1_TREE_HASH {
            Cat::new_v1(args, CatSolution::try_from(&solution).unwrap())
        } else {
            Cat::new(args, CatSolution::try_from(&solution).unwrap())
        };
        assert_eq!(
            cat.curried_tree_hash().unwrap(),
            Bytes32::const_hex(expected_hash)
        );
        let (_, expected) = puzzle
            .run(INFINITE_COST, 0, &Program::new(solution.clone()))
            .unwrap();
        assert_eq!(
            cat.run(CatSolution::try_from(&solution).unwrap()).unwrap(),
            expected
        );
    }
}
