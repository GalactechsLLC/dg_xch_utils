#[cfg(test)]
mod tests {
    use dg_xch_core::blockchain::coin::Coin;
    use dg_xch_core::blockchain::coin_spend::CoinSpend;
    use dg_xch_core::blockchain::sized_bytes::Bytes32;
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::program::Program;
    use std::vec;

    #[test]
    fn test_compute_additions_with_cost_success() {
        let puzzle_reveal = assemble_text("(c (c (q . 51) (c 2 (q 1))) ())").unwrap();
        let puzzle_reveal_hash = puzzle_reveal.to_program().tree_hash();

        let coin = Coin {
            parent_coin_info: Bytes32::default(),
            puzzle_hash: puzzle_reveal_hash,
            amount: 1,
        };

        let solution: Program = Program::to(vec![puzzle_reveal_hash]);
        let cs = CoinSpend {
            coin,
            puzzle_reveal,
            solution: solution.into(),
        };

        let max_cost = 1000000u64;

        let (additions, _costs) = cs.compute_additions_with_cost(max_cost).unwrap();
        assert_eq!(additions.len(), 1);
        assert_eq!(additions[0].parent_coin_info, coin.coin_id());
        assert_eq!(additions[0].puzzle_hash, puzzle_reveal_hash);
        assert_eq!(additions[0].amount, 1);
    }

    #[test]
    fn test_compute_additions_with_cost_cost_exceeds_max() {
        todo!()
    }

    #[test]
    fn test_compute_additions_with_cost_atoms_empty() {
        todo!()
    }

    #[test]
    fn test_compute_additions_with_cost_invalid_number_of_atoms() {
        todo!()
    }

    #[test]
    fn test_compute_additions_with_cost_amount_conversion_failure() {
        todo!()
    }

    #[test]
    fn test_compute_additions_with_cost_non_create_coin_condition() {
        todo!()
    }

    #[test]
    fn test_coinspend_additions_success() {
        todo!()
    }

    #[test]
    fn test_coinspend_reserved_fee() {
        todo!()
    }
}
