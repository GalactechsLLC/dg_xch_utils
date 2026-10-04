// Canonical puzzle artifacts from chia_puzzles ff0016e10fc986cde91b2bec3453264f0f6cb383.
// Sources and license are retained in programs/; DG compilation is checked in tests.
use dg_parser_macro::parse_program_hex;

parse_program_hex!(CAT_PUZZLE, "src/programs/cat_puzzles/cat_v2.clsp.hex");

parse_program_hex!(
    DELEGATED_TAIL,
    "src/programs/cat_puzzles/delegated_tail.clsp.hex"
);

parse_program_hex!(
    EVERYTHING_WITH_SIGNATURE,
    "src/programs/cat_puzzles/everything_with_signature.clsp.hex"
);

parse_program_hex!(
    EVERYTHING_WITH_SINGLETON,
    "src/programs/cat_puzzles/everything_with_singleton.clsp.hex"
);

parse_program_hex!(
    GENESIS_BY_COIN_ID_OR_SINGLETON,
    "src/programs/cat_puzzles/genesis_by_coin_id_or_singleton.clsp.hex"
);

parse_program_hex!(
    GENESIS_BY_COIN_ID,
    "src/programs/cat_puzzles/genesis_by_coin_id.clsp.hex"
);

parse_program_hex!(
    GENESIS_BY_PUZZLE_HASH,
    "src/programs/cat_puzzles/genesis_by_puzzle_hash.clsp.hex"
);

parse_program_hex!(DAO_CAT_EVE, "src/programs/dao_puzzles/dao_cat_eve.clsp.hex");

parse_program_hex!(
    DAO_CAT_LAUNCHER,
    "src/programs/dao_puzzles/dao_cat_launcher.clsp.hex"
);

parse_program_hex!(
    DAO_FINISHED_STATE,
    "src/programs/dao_puzzles/dao_finished_state.clsp.hex"
);

parse_program_hex!(DAO_LOCKUP, "src/programs/dao_puzzles/dao_lockup.clsp.hex");

parse_program_hex!(
    DAO_PROPOSAL_TIMER,
    "src/programs/dao_puzzles/dao_proposal_timer.clsp.hex"
);

parse_program_hex!(
    DAO_PROPOSAL_VALIDATOR,
    "src/programs/dao_puzzles/dao_proposal_validator.clsp.hex"
);

parse_program_hex!(
    DAO_PROPOSAL,
    "src/programs/dao_puzzles/dao_proposal.clsp.hex"
);

parse_program_hex!(
    DAO_SPEND_P2_SINGLETON,
    "src/programs/dao_puzzles/dao_spend_p2_singleton_v2.clsp.hex"
);

parse_program_hex!(
    DAO_TREASURY,
    "src/programs/dao_puzzles/dao_treasury.clsp.hex"
);

parse_program_hex!(
    DAO_UPDATE_PROPOSAL,
    "src/programs/dao_puzzles/dao_update_proposal.clsp.hex"
);

parse_program_hex!(
    DID_INNERPUZ,
    "src/programs/did_puzzles/did_innerpuz.clsp.hex"
);

parse_program_hex!(
    GRAFTROOT_DL_OFFERS,
    "src/programs/dl_puzzles/graftroot_dl_offers.clsp.hex"
);

parse_program_hex!(
    CREATE_NFT_LAUNCHER_FROM_DID,
    "src/programs/nft_puzzles/create_nft_launcher_from_did.clsp.hex"
);

pub use self::{
    CREATE_NFT_LAUNCHER_FROM_DID_HEX as NFT_INTERMEDIATE_LAUNCHER_HEX,
    CREATE_NFT_LAUNCHER_FROM_DID_PROGRAM as NFT_INTERMEDIATE_LAUNCHER_PROGRAM,
    CREATE_NFT_LAUNCHER_FROM_DID_SEXP as NFT_INTERMEDIATE_LAUNCHER_SEXP,
    CREATE_NFT_LAUNCHER_FROM_DID_SRC as NFT_INTERMEDIATE_LAUNCHER_SRC,
    CREATE_NFT_LAUNCHER_FROM_DID_TREE_HASH as NFT_INTERMEDIATE_LAUNCHER_TREE_HASH,
};

parse_program_hex!(
    NFT_METADATA_UPDATER_DEFAULT,
    "src/programs/nft_puzzles/nft_metadata_updater_default.clsp.hex"
);

parse_program_hex!(
    NFT_METADATA_UPDATER_UPDATEABLE,
    "src/programs/nft_puzzles/nft_metadata_updater_updateable.clsp.hex"
);

parse_program_hex!(
    NFT_OWNERSHIP_LAYER,
    "src/programs/nft_puzzles/nft_ownership_layer.clsp.hex"
);

parse_program_hex!(
    NFT_OWNERSHIP_TRANSFER_PROGRAM_ONE_WAY_CLAIM_WITH_ROYALTIES,
    "src/programs/nft_puzzles/nft_ownership_transfer_program_one_way_claim_with_royalties.clsp.hex"
);

parse_program_hex!(
    NFT_STATE_LAYER,
    "src/programs/nft_puzzles/nft_state_layer.clsp.hex"
);

parse_program_hex!(
    CONDITIONS_W_FEE_ANNOUNCE,
    "src/programs/vc_puzzles/cr_puzzles/conditions_w_fee_announce.clsp.hex"
);

parse_program_hex!(
    CREDENTIAL_RESTRICTION,
    "src/programs/vc_puzzles/cr_puzzles/credential_restriction.clsp.hex"
);

parse_program_hex!(
    FLAG_PROOFS_CHECKER,
    "src/programs/vc_puzzles/cr_puzzles/flag_proofs_checker.clsp.hex"
);

parse_program_hex!(
    COVENANT_LAYER,
    "src/programs/vc_puzzles/covenant_layer.clsp.hex"
);

parse_program_hex!(
    EML_COVENANT_MORPHER,
    "src/programs/vc_puzzles/eml_covenant_morpher.clsp.hex"
);

parse_program_hex!(
    EML_TRANSFER_PROGRAM_COVENANT_ADAPTER,
    "src/programs/vc_puzzles/eml_transfer_program_covenant_adapter.clsp.hex"
);

parse_program_hex!(
    EML_UPDATE_METADATA_WITH_DID,
    "src/programs/vc_puzzles/eml_update_metadata_with_DID.clsp.hex"
);

parse_program_hex!(
    EXIGENT_METADATA_LAYER,
    "src/programs/vc_puzzles/exigent_metadata_layer.clsp.hex"
);

parse_program_hex!(
    P2_ANNOUNCED_DELEGATED_PUZZLE,
    "src/programs/vc_puzzles/p2_announced_delegated_puzzle.clsp.hex"
);

parse_program_hex!(
    STANDARD_VC_REVOCATION_PUZZLE,
    "src/programs/vc_puzzles/standard_vc_revocation_puzzle.clsp.hex"
);

parse_program_hex!(
    STD_PARENT_MORPHER,
    "src/programs/vc_puzzles/std_parent_morpher.clsp.hex"
);

parse_program_hex!(
    REVOCATION_LAYER,
    "src/programs/vc_puzzles/revocation_layer.clsp.hex"
);

parse_program_hex!(
    ACS_TRANSFER_PROGRAM,
    "src/programs/vc_puzzles/acs_transfer_program.clsp.hex"
);

parse_program_hex!(
    AUGMENTED_CONDITION,
    "src/programs/augmented_condition.clsp.hex"
);

parse_program_hex!(NOTIFICATION, "src/programs/notification.clsp.hex");

parse_program_hex!(P2_1_OF_N, "src/programs/p2_1_of_n.clsp.hex");

parse_program_hex!(P2_CONDITIONS, "src/programs/p2_conditions.clsp.hex");

parse_program_hex!(
    P2_DELEGATED_CONDITIONS,
    "src/programs/p2_delegated_conditions.clsp.hex"
);

parse_program_hex!(
    P2_DELEGATED_PUZZLE_OR_HIDDEN_PUZZLE,
    "src/programs/p2_delegated_puzzle_or_hidden_puzzle.clsp.hex"
);

parse_program_hex!(
    P2_DELEGATED_PUZZLE,
    "src/programs/p2_delegated_puzzle.clsp.hex"
);

parse_program_hex!(
    P2_M_OF_N_DELEGATE_DIRECT,
    "src/programs/p2_m_of_n_delegate_direct.clsp.hex"
);

parse_program_hex!(P2_PARENT, "src/programs/p2_parent.clsp.hex");

parse_program_hex!(
    P2_SINGLETON_AGGREGATOR,
    "src/programs/p2_singleton_aggregator.clsp.hex"
);

parse_program_hex!(
    P2_SINGLETON_OR_DELAYED_PUZHASH,
    "src/programs/p2_singleton_or_delayed_puzhash.clsp.hex"
);

parse_program_hex!(
    P2_SINGLETON_VIA_DELEGATED_PUZZLE,
    "src/programs/p2_singleton_via_delegated_puzzle.clsp.hex"
);

parse_program_hex!(P2_SINGLETON, "src/programs/p2_singleton.clsp.hex");

parse_program_hex!(P2_PUZZLE_HASH, "src/programs/p2_puzzle_hash.clsp.hex");

parse_program_hex!(
    SETTLEMENT_PAYMENT,
    "src/programs/settlement_payments.clsp.hex"
);

parse_program_hex!(
    SINGLETON_LAUNCHER,
    "src/programs/singleton_launcher.clsp.hex"
);

parse_program_hex!(
    SINGLETON_TOP_LAYER_V1_1,
    "src/programs/singleton_top_layer_v1_1.clsp.hex"
);

parse_program_hex!(
    SINGLETON_TOP_LAYER,
    "src/programs/singleton_top_layer.clsp.hex"
);

parse_program_hex!(
    ONE_OF_N,
    "src/programs/mips_puzzles/architecture_puzzles/1_of_n.clsp.hex"
);

parse_program_hex!(
    M_OF_N,
    "src/programs/mips_puzzles/architecture_puzzles/m_of_n.clsp.hex"
);

parse_program_hex!(
    N_OF_N,
    "src/programs/mips_puzzles/architecture_puzzles/n_of_n.clsp.hex"
);

parse_program_hex!(
    DELEGATED_PUZZLE_FEEDER,
    "src/programs/mips_puzzles/architecture_puzzles/delegated_puzzle_feeder.clsp.hex"
);

parse_program_hex!(
    RESTRICTIONS,
    "src/programs/mips_puzzles/architecture_puzzles/restrictions.clsp.hex"
);

parse_program_hex!(
    BLS_MEMBER_PUZZLE_ASSERT,
    "src/programs/mips_puzzles/member_puzzles/bls_member_puzzle_assert.clsp.hex"
);

parse_program_hex!(
    BLS_MEMBER,
    "src/programs/mips_puzzles/member_puzzles/bls_member.clsp.hex"
);

parse_program_hex!(
    BLS_WITH_TAPROOT_MEMBER_PUZZLE_ASSERT,
    "src/programs/mips_puzzles/member_puzzles/bls_with_taproot_member_puzzle_assert.clsp.hex"
);

parse_program_hex!(
    BLS_WITH_TAPROOT_MEMBER,
    "src/programs/mips_puzzles/member_puzzles/bls_with_taproot_member.clsp.hex"
);

parse_program_hex!(
    FIXED_PUZZLE_MEMBER,
    "src/programs/mips_puzzles/member_puzzles/fixed_puzzle_member.clsp.hex"
);

parse_program_hex!(
    PASSKEY_MEMBER_PUZZLE_ASSERT,
    "src/programs/mips_puzzles/member_puzzles/passkey_member_puzzle_assert.clsp.hex"
);

parse_program_hex!(
    PASSKEY_MEMBER,
    "src/programs/mips_puzzles/member_puzzles/passkey_member.clsp.hex"
);

parse_program_hex!(
    SECP256K1_MEMBER_PUZZLE_ASSERT,
    "src/programs/mips_puzzles/member_puzzles/secp256k1_member_puzzle_assert.clsp.hex"
);

parse_program_hex!(
    SECP256K1_MEMBER,
    "src/programs/mips_puzzles/member_puzzles/secp256k1_member.clsp.hex"
);

parse_program_hex!(
    SECP256R1_MEMBER_PUZZLE_ASSERT,
    "src/programs/mips_puzzles/member_puzzles/secp256r1_member_puzzle_assert.clsp.hex"
);

parse_program_hex!(
    SECP256R1_MEMBER,
    "src/programs/mips_puzzles/member_puzzles/secp256r1_member.clsp.hex"
);

parse_program_hex!(
    SINGLETON_MEMBER_WITH_MODE,
    "src/programs/mips_puzzles/member_puzzles/singleton_member_with_mode.clsp.hex"
);

parse_program_hex!(
    SINGLETON_MEMBER,
    "src/programs/mips_puzzles/member_puzzles/singleton_member.clsp.hex"
);

parse_program_hex!(
    FORCE_1_OF_2_W_RESTRICTED_VARIABLE,
    "src/programs/mips_puzzles/restriction_puzzles/wrappers/force_1_of_2_w_restricted_variable.clsp.hex"
);

parse_program_hex!(
    FORCE_ASSERT_COIN_ANNOUNCEMENT,
    "src/programs/mips_puzzles/restriction_puzzles/wrappers/force_assert_coin_announcement.clsp.hex"
);

parse_program_hex!(
    FORCE_COIN_MESSAGE,
    "src/programs/mips_puzzles/restriction_puzzles/wrappers/force_coin_message.clsp.hex"
);

parse_program_hex!(
    PREVENT_CONDITION_OPCODE,
    "src/programs/mips_puzzles/restriction_puzzles/wrappers/prevent_condition_opcode.clsp.hex"
);

parse_program_hex!(
    PREVENT_MULTIPLE_CREATE_COINS,
    "src/programs/mips_puzzles/restriction_puzzles/wrappers/prevent_multiple_create_coins.clsp.hex"
);

parse_program_hex!(
    TIMELOCK,
    "src/programs/mips_puzzles/restriction_puzzles/wrappers/timelock.clsp.hex"
);

parse_program_hex!(
    ADD_DPUZ_WRAPPER,
    "src/programs/mips_puzzles/restriction_puzzles/add_dpuz_wrapper.clsp.hex"
);

parse_program_hex!(
    ENFORCE_DPUZ_WRAPPERS,
    "src/programs/mips_puzzles/restriction_puzzles/enforce_dpuz_wrappers.clsp.hex"
);

parse_program_hex!(
    POOL_MEMBER_INNERPUZ,
    "src/programs/pool_puzzles/pool_member_innerpuz.clsp.hex"
);

parse_program_hex!(
    POOL_WAITINGROOM_INNERPUZ,
    "src/programs/pool_puzzles/pool_waitingroom_innerpuz.clsp.hex"
);

parse_program_hex!(
    CHIALISP_DESERIALISATION,
    "src/programs/consensus_puzzles/chialisp_deserialisation.clsp.hex"
);

parse_program_hex!(
    ROM_BOOTSTRAP_GENERATOR,
    "src/programs/consensus_puzzles/rom_bootstrap_generator.clsp.hex"
);

parse_program_hex!(
    BLOCK_PROGRAM_ZERO,
    "src/programs/full_node_puzzles/block_program_zero.clsp.hex"
);

parse_program_hex!(
    DECOMPRESS_COIN_SPEND_ENTRY_WITH_PREFIX,
    "src/programs/full_node_puzzles/decompress_coin_spend_entry_with_prefix.clsp.hex"
);

parse_program_hex!(
    DECOMPRESS_COIN_SPEND_ENTRY,
    "src/programs/full_node_puzzles/decompress_coin_spend_entry.clsp.hex"
);

parse_program_hex!(
    DECOMPRESS_PUZZLE,
    "src/programs/full_node_puzzles/decompress_puzzle.clsp.hex"
);
