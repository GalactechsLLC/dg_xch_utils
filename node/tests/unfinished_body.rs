mod common;

use common::load_full_block;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::unfinished_block::UnfinishedBlock;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::consensus::block_generator::{
    transactions_generator_root, transactions_info_hash,
};
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::errors::ChiaError;
use dg_xch_node::{NativePrimitives, NodeError, validate_unfinished_block_body};

const HEIGHT: u32 = 5_000_004;
// The previous transaction block's height — the SF9 body-rule key (pre-SF9 here).
const PREV_TX: u32 = 5_000_000;

// The UnfinishedBlock a peer would have relayed for real mainnet block 5,000,004: strip the
// infusion-point VDFs, keep everything the farmer signed.
fn unfinished_5000004() -> UnfinishedBlock {
    let fb: FullBlock = load_full_block(HEIGHT);
    UnfinishedBlock {
        finished_sub_slots: fb.finished_sub_slots.clone(),
        reward_chain_block: fb.reward_chain_block.get_unfinished(),
        challenge_chain_sp_proof: fb.challenge_chain_sp_proof.clone(),
        reward_chain_sp_proof: fb.reward_chain_sp_proof.clone(),
        foliage: fb.foliage,
        foliage_transaction_block: fb.foliage_transaction_block,
        transactions_info: fb.transactions_info.clone(),
        transactions_generator: fb.transactions_generator.clone(),
        transactions_generator_ref_list: fb.transactions_generator_ref_list.clone(),
    }
}

// Swap in `generator`, forging the transactions_info commitment chain to it (generator_root,
// then the foliage transaction block's transactions_info_hash) — the shape an attacker who
// FARMED the block (and so holds the plot key) can sign for real, per the live ban vector.
fn with_forged_generator(mut ub: UnfinishedBlock, generator: SerializedProgram) -> UnfinishedBlock {
    let mut ti = ub
        .transactions_info
        .take()
        .expect("tx block has transactions_info");
    ti.generator_root = transactions_generator_root(&generator);
    rebind(&mut ub, ti);
    ub.transactions_generator = Some(generator);
    ub
}

// Rebind a (tampered) transactions_info into the foliage transaction block's commitment.
fn rebind(
    ub: &mut UnfinishedBlock,
    ti: dg_xch_core::blockchain::transactions_info::TransactionsInfo,
) {
    let mut ftb = ub
        .foliage_transaction_block
        .expect("tx block has foliage_transaction_block");
    ftb.transactions_info_hash = transactions_info_hash(&ti).expect("ti hashes");
    ub.foliage_transaction_block = Some(ftb);
    ub.transactions_info = Some(ti);
}

fn validate(
    ub: &UnfinishedBlock,
) -> Result<
    Option<dg_xch_core::blockchain::spend_bundle_conditions::SpendBundleConditions>,
    NodeError,
> {
    validate_unfinished_block_body(&NativePrimitives, &MAINNET, ub, &[], HEIGHT, PREV_TX)
}

#[test]
fn undeserializable_generator_is_rejected_at_the_run() {
    let ub = with_forged_generator(
        unfinished_5000004(),
        SerializedProgram::from_hex("fffefd").expect("hex"),
    );
    let err = validate(&ub).expect_err("must reject");
    assert!(
        matches!(err, NodeError::Consensus(ChiaError::InvalidBlockSolution)),
        "deserialize failure surfaces from the generator run (got {err:?})"
    );
}

// GENERATOR_RUNTIME_ERROR species 2 — a well-formed program that raises when run: `(x)`.
#[test]
fn raising_generator_is_rejected_at_the_run() {
    let ub = with_forged_generator(
        unfinished_5000004(),
        SerializedProgram::from_hex("ff0880").expect("hex"),
    );
    let err = validate(&ub).expect_err("must reject");
    assert!(
        matches!(err, NodeError::Consensus(ChiaError::GeneratorRuntimeError)),
        "a CLVM raise surfaces as the generator runtime error (got {err:?})"
    );
}

// INVALID_BLOCK_COST — the claim overstates the true cost: the run finishes under budget and
// the exact-equality rule (body rule 9) rejects.
#[test]
fn overstated_cost_claim_is_rejected() {
    let mut ub = unfinished_5000004();
    let mut ti = ub.transactions_info.take().expect("ti");
    ti.cost += 1;
    rebind(&mut ub, ti);
    let err = validate(&ub).expect_err("must reject");
    assert!(
        matches!(err, NodeError::Consensus(ChiaError::InvalidBlockCost)),
        "an inexact cost claim is INVALID_BLOCK_COST (got {err:?})"
    );
}

// BLOCK_COST_EXCEEDS_MAX — the claim understates the true cost: the run budget is
// `min(MAX_BLOCK_COST_CLVM, claimed)`, so execution blows the claimed budget DURING the run,
// burning at most the claimed cost of our CPU (the DoS bound the clamp exists for).
#[test]
fn understated_cost_claim_fails_during_the_run() {
    let mut ub = unfinished_5000004();
    let mut ti = ub.transactions_info.take().expect("ti");
    ti.cost -= 1;
    rebind(&mut ub, ti);
    let err = validate(&ub).expect_err("must reject");
    assert!(
        matches!(err, NodeError::Consensus(ChiaError::BlockCostExceedsMax)),
        "a run over the claimed budget is BLOCK_COST_EXCEEDS_MAX (got {err:?})"
    );
}

#[test]
fn honest_mainnet_5000004_validates_with_exact_cost() {
    let ub = unfinished_5000004();
    let claimed = ub.transactions_info.as_ref().expect("ti").cost;
    let conds = validate(&ub)
        .expect("honest block validates")
        .expect("generator-bearing block yields conditions");
    assert_eq!(
        conds.cost, claimed,
        "executed cost equals the mainnet claim"
    );
    assert!(!conds.spends.is_empty(), "real block spends parsed");
}

// The empty-generator fast path (the own-farmed non-transaction shape): nothing to run,
// nothing to reject — conds stays None.
#[test]
fn non_tx_unfinished_block_passes_untouched() {
    let mut ub = unfinished_5000004();
    ub.transactions_info = None;
    ub.foliage_transaction_block = None;
    ub.transactions_generator = None;
    ub.transactions_generator_ref_list = Vec::new();
    let out = validate(&ub).expect("non-tx block passes");
    assert!(out.is_none(), "no conditions for a non-transaction block");
}

// A non-transaction block carrying a generator is structurally invalid (the server-level red
// test drives this same species through the full relay path).
#[test]
fn non_tx_unfinished_block_with_generator_is_rejected() {
    let mut ub = unfinished_5000004();
    ub.transactions_info = None;
    ub.foliage_transaction_block = None;
    ub.transactions_generator = Some(SerializedProgram::from_hex("ff0880").expect("hex"));
    ub.transactions_generator_ref_list = Vec::new();
    let err = validate(&ub).expect_err("must reject");
    assert!(
        matches!(
            err,
            NodeError::Consensus(ChiaError::InvalidTransactionsGeneratorHash)
        ),
        "a generator on a non-tx block is INVALID_TRANSACTIONS_GENERATOR_HASH (got {err:?})"
    );
}
