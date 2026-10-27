use dg_xch_core::clvm::utils::{
    CANONICAL_INTS, COST_CONDITIONS, DISABLE_OP, ENABLE_KECCAK_OPS_OUTSIDE_FORK, LIMITS,
    NEW_COST_MODEL, RELAXED_BLS,
};
use dg_xch_core::consensus::block_generator::BlockGeneratorFlags;
use dg_xch_core::consensus::constants::{ConsensusConstants, MAINNET};

/// The expected ladder, transcribed from the rules rather than from our implementation, so the
/// two disagreeing means one is wrong.
fn expected(constants: &ConsensusConstants, height: u32) -> (u32, bool) {
    let mut flags = 0u32;
    if height >= constants.hard_fork2_height {
        flags |= ENABLE_KECCAK_OPS_OUTSIDE_FORK | COST_CONDITIONS | NEW_COST_MODEL | RELAXED_BLS;
    } else if height >= constants.soft_fork8_height {
        flags |= DISABLE_OP;
    }
    if height >= constants.soft_fork9_height {
        flags |= CANONICAL_INTS;
        if height < constants.hard_fork2_height {
            flags |= LIMITS;
        }
    }
    (flags, height >= constants.hard_fork_height)
}

/// Every height where behaviour can change, plus the block on either side of it.
fn boundary_heights(c: &ConsensusConstants) -> Vec<u32> {
    let mut out = vec![0u32, 1];
    for h in [
        c.hard_fork_height,
        c.soft_fork8_height,
        c.soft_fork9_height,
        c.hard_fork2_height,
    ] {
        out.push(h.saturating_sub(1));
        out.push(h);
        out.push(h.saturating_add(1));
    }
    // Points inside each window, including the one the old ladder got wrong.
    out.push(4_671_894); // a real fixture: pre-hard-fork, ROM path
    out.push(6_000_000); // inside the 3.16M-block window that was mis-keyed
    out.push(7_500_000);
    out.push(9_179_161); // a real fixture: post soft fork 9
    out.sort_unstable();
    out.dedup();
    out
}

#[test]
fn the_ladder_matches_chia_at_every_boundary() {
    let mut checked = 0usize;
    for height in boundary_heights(&MAINNET) {
        let got = BlockGeneratorFlags::for_height(&MAINNET, height);
        let (want_flags, want_simple) = expected(&MAINNET, height);
        assert_eq!(
            got.clvm_flags, want_flags,
            "height {height}: clvm flags {:#010x} != expected {want_flags:#010x}",
            got.clvm_flags
        );
        assert_eq!(
            got.simple_generator, want_simple,
            "height {height}: simple_generator {} != expected {want_simple}",
            got.simple_generator
        );
        checked += 1;
    }
    eprintln!("  {checked} mainnet boundary heights match the expected ladder");
}

#[test]
fn the_ladder_matches_chia_when_the_soft_forks_are_at_different_heights() {
    // Mainnet puts soft forks 8 and 9 at the same height, hiding any confusion between them.
    // Testnet11 separates them, which is where keying an SF9 rule on SF8 diverges.
    let mut c = MAINNET;
    c.soft_fork8_height = 3_755_000;
    c.soft_fork9_height = 3_924_000;

    for height in boundary_heights(&c) {
        let got = BlockGeneratorFlags::for_height(&c, height);
        let (want_flags, want_simple) = expected(&c, height);
        assert_eq!(
            got.clvm_flags, want_flags,
            "height {height} (split soft forks): {:#010x} != {want_flags:#010x}",
            got.clvm_flags
        );
        assert_eq!(
            got.simple_generator, want_simple,
            "height {height}: simple_generator"
        );
    }

    // Spell out the window explicitly: between the two soft forks, DISABLE_OP is on and the soft
    // fork 9 set is not.
    let between = BlockGeneratorFlags::for_height(&c, 3_800_000);
    assert_ne!(
        between.clvm_flags & DISABLE_OP,
        0,
        "DISABLE_OP should be on after soft fork 8"
    );
    assert_eq!(
        between.clvm_flags & LIMITS,
        0,
        "LIMITS belongs to soft fork 9, not 8"
    );
    assert_eq!(
        between.clvm_flags & CANONICAL_INTS,
        0,
        "CANONICAL_INTS belongs to soft fork 9"
    );
    assert!(
        !between.simple_generator,
        "the simple generator arrives with soft fork 9"
    );
    eprintln!("  split soft-fork heights hold; the SF8/SF9 window is distinguished");
}

#[test]
fn hard_fork_two_supersedes_the_soft_fork_eight_and_nine_rules() {
    // Hard fork 2 stops disabling modpow and stops applying the operand caps; its cost model
    // replaces both.
    let mut c = MAINNET;
    c.hard_fork2_height = 9_000_000;

    let before = BlockGeneratorFlags::for_height(&c, 8_999_999);
    let after = BlockGeneratorFlags::for_height(&c, 9_000_000);

    assert_ne!(
        before.clvm_flags & DISABLE_OP,
        0,
        "modpow is disabled before hard fork 2"
    );
    assert_eq!(
        after.clvm_flags & DISABLE_OP,
        0,
        "hard fork 2 re-enables modpow"
    );

    assert_ne!(
        before.clvm_flags & LIMITS,
        0,
        "operand caps apply before hard fork 2"
    );
    assert_eq!(
        after.clvm_flags & LIMITS,
        0,
        "the bounded cost model subsumes the caps"
    );

    assert_eq!(before.clvm_flags & NEW_COST_MODEL, 0);
    assert_ne!(after.clvm_flags & NEW_COST_MODEL, 0);
    assert_ne!(
        after.clvm_flags & COST_CONDITIONS,
        0,
        "flat condition costs arrive with hard fork 2"
    );
    assert_ne!(
        after.clvm_flags & ENABLE_KECCAK_OPS_OUTSIDE_FORK,
        0,
        "keccak leaves the softfork guard at hard fork 2"
    );

    // Soft fork 9 rules that are NOT superseded stay on.
    assert_ne!(
        after.clvm_flags & CANONICAL_INTS,
        0,
        "canonical ints survive hard fork 2"
    );
    assert!(
        after.simple_generator,
        "the simple generator survives hard fork 2"
    );
    eprintln!("  hard fork 2 supersedes exactly the rules it should");
}

#[test]
fn a_flag_is_never_enabled_before_its_fork() {
    // Hard fork 1 contributes no CLVM flag of its own, so nothing may be set until soft fork 8.
    let hf1 = MAINNET.hard_fork_height;
    let at_hf1 = BlockGeneratorFlags::for_height(&MAINNET, hf1);
    assert_eq!(
        at_hf1.clvm_flags, 0,
        "hard fork 1 contributes no CLVM flag, but {:#010x} was set",
        at_hf1.clvm_flags
    );
    assert!(
        at_hf1.simple_generator,
        "the simple generator mode arrives exactly at hard fork 1"
    );

    // And nothing changes anywhere in the window until soft fork 8.
    for height in [hf1 + 1, 6_000_000, 7_500_000, MAINNET.soft_fork8_height - 1] {
        let f = BlockGeneratorFlags::for_height(&MAINNET, height);
        assert_eq!(
            f.clvm_flags, 0,
            "height {height}: no flag is active in this window"
        );
        assert!(
            f.simple_generator,
            "height {height}: the mode stays on after hard fork 1"
        );
    }
    eprintln!("  the hard-fork-1 to soft-fork-8 window is flag-free, as chia has it");
}

#[test]
fn falcon_operator_activation_does_not_change_bls_consensus_flags() {
    use dg_xch_core::{clvm::falcon_ops::ENABLE_FALCON_OPS, signatures::SignatureProfile};
    let mut falcon = MAINNET;
    falcon.signature_profile = SignatureProfile::FnDsa512V2;
    for height in boundary_heights(&MAINNET) {
        let bls = BlockGeneratorFlags::for_height(&MAINNET, height).clvm_flags;
        assert_eq!(bls & ENABLE_FALCON_OPS, 0);
        assert_eq!(
            BlockGeneratorFlags::for_height(&falcon, height).clvm_flags,
            bls | ENABLE_FALCON_OPS
        );
    }
}
