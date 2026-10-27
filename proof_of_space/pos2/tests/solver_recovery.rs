use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_pos2::{
    bits::expand_bits, chainer::Chain, params::ProofParams, plotting::PlotLimits, solver::solve,
    validator::ProofValidator,
};
use std::sync::atomic::AtomicBool;

// Exercise the ordinary HashEngine path independently of the fused CPU scan.
struct OrdinaryCpu(dg_xch_pos2::compute::CpuHasher);

impl dg_xch_pos2::compute::HashEngine for OrdinaryCpu {
    fn hash(
        &mut self,
        inputs: &[[u32; 4]],
        rounds: u32,
        cancelled: &AtomicBool,
    ) -> Result<Vec<[u32; 4]>, std::io::Error> {
        dg_xch_pos2::compute::HashEngine::hash(&mut self.0, inputs, rounds, cancelled)
    }
}

// Quality chains read from a deployed k28 strength-2 testnet plot. No plot file
// or farmer private key is needed to reproduce the complete recovery workload.
const PLOT_ID: [u8; 32] = [
    186, 142, 223, 240, 136, 133, 250, 255, 64, 64, 197, 91, 121, 13, 234, 132, 181, 195, 215, 7,
    195, 171, 223, 214, 73, 75, 194, 100, 255, 29, 137, 88,
];
const CHAINS: [(u32, [u64; 16]); 2] = [
    (
        0,
        [
            17330104279056270,
            47019538934024438,
            17864149320431049,
            60886477469859522,
            17330098806463423,
            47019539330133480,
            17864157198454339,
            60886470531748089,
            17330103782406850,
            47019546971280328,
            17864152903969906,
            60886476661959685,
            17330102441092880,
            47019543393021580,
            17864145429244192,
            60886483483092343,
        ],
    ),
    (
        6,
        [
            50520462417491300,
            55915501061176031,
            9351679203399349,
            42713010621481902,
            50520458582753028,
            55915508502057439,
            9351681815406490,
            42713011638136589,
            50520454468476749,
            55915506153540107,
            9351684392786057,
            42713006005745293,
            50520448667376822,
            55915499225138595,
            9351677000322749,
            42713007733702987,
        ],
    ),
];

#[test]
#[ignore = "full k28 CPU recovery; run with --release --test solver_recovery -- --ignored"]
fn real_k28_chains_recover_under_256_mib_and_enforce_limits() {
    rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap()
        .install(|| {
            let params = ProofParams::new(Bytes32::from(PLOT_ID), 28, 2, true).unwrap();
            let validator = ProofValidator::new(params.clone()).unwrap();
            let limits = PlotLimits {
                memory_bytes: 256 * 1024 * 1024,
                max_entries: 310_000_000,
                max_work: 10_000_000_000,
            };
            for (index, fragments) in CHAINS {
                let mut challenge = [0; 32];
                challenge[..4].copy_from_slice(&index.to_le_bytes());
                let challenge = Bytes32::from(challenge);
                let chain = Chain { fragments };
                let cancelled = AtomicBool::new(false);
                let proof = solve(&params, &chain, challenge, limits, &cancelled).unwrap();
                let ordinary = dg_xch_pos2::solver::solve_with_engine(
                    &params,
                    &chain,
                    challenge,
                    limits,
                    &cancelled,
                    &mut OrdinaryCpu(dg_xch_pos2::compute::CpuHasher::new(&params)),
                )
                .unwrap();
                assert_eq!(
                    proof, ordinary,
                    "fused and ordinary scans must be byte-identical"
                );
                let xs: [u32; 128] = expand_bits(&proof, 28).unwrap().try_into().unwrap();
                assert_eq!(
                    validator.validate_full_proof(&xs, challenge),
                    Some(fragments)
                );
                assert!(
                    solve(
                        &params,
                        &chain,
                        challenge,
                        PlotLimits {
                            memory_bytes: 32 * 1024 * 1024,
                            ..limits
                        },
                        &cancelled
                    )
                    .is_err()
                );
                assert!(
                    solve(
                        &params,
                        &chain,
                        challenge,
                        PlotLimits {
                            max_entries: 1,
                            ..limits
                        },
                        &cancelled
                    )
                    .is_err()
                );
                assert!(
                    solve(
                        &params,
                        &chain,
                        challenge,
                        PlotLimits {
                            max_work: 1,
                            ..limits
                        },
                        &cancelled
                    )
                    .is_err()
                );
                assert_eq!(
                    solve(&params, &chain, challenge, limits, &AtomicBool::new(true))
                        .unwrap_err()
                        .kind(),
                    std::io::ErrorKind::Interrupted
                );
            }
        });
}
