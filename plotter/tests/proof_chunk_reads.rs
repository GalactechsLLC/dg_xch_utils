use dg_xch_plotter::{PlotRequest, PoolBinding, reader::PlotReader};
use dg_xch_pos2::{
    chainer::SearchLimits, params::Range, plotting::PlotLimits, validator::ProofValidator,
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

struct CountedFile {
    file: File,
    bytes: Arc<AtomicU64>,
}
impl Read for CountedFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.file.read(buffer)?;
        self.bytes.fetch_add(count as u64, Ordering::Relaxed);
        Ok(count)
    }
}
impl Seek for CountedFile {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(position)
    }
}

#[test]
#[ignore = "constructs a k18 plot; run in release mode"]
fn proof_validation_reuses_chunks_and_preserves_membership_checks() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("proof.plot");
    let request = PlotRequest {
        farmer_public_key: blst::min_pk::SecretKey::key_gen_v3(&[7; 32], &[])
            .unwrap()
            .sk_to_pk()
            .to_bytes(),
        pool: PoolBinding::Contract([8; 32]),
        k: 18,
        strength: 2,
        index: 256,
        meta_group: 3,
        testnet: true,
    };
    let limits = PlotLimits::default();
    let cancelled = AtomicBool::new(false);
    dg_xch_plotter::create(&request, &path, limits, &cancelled).unwrap();
    let bytes = Arc::new(AtomicU64::new(0));
    let mut reader = PlotReader::from_reader(
        CountedFile {
            file: File::open(&path).unwrap(),
            bytes: bytes.clone(),
        },
        true,
        limits.memory_bytes,
    )
    .unwrap();
    for attempt in 0u16..256 {
        let mut challenge = [0u8; 32];
        challenge[..2].copy_from_slice(&attempt.to_le_bytes());
        let challenge = challenge.into();
        let chains = reader
            .qualities(
                challenge,
                SearchLimits {
                    max_hashes: 100_000_000,
                    max_results: 32,
                },
                &cancelled,
            )
            .unwrap();
        if let Some(chain) = chains.first() {
            bytes.store(0, Ordering::Relaxed);
            for fragment in chain.fragments {
                assert!(
                    reader
                        .fragments_in_range(
                            Range {
                                start: fragment,
                                end: fragment
                            },
                            &cancelled
                        )
                        .unwrap()
                        .binary_search(&fragment)
                        .is_ok()
                );
            }
            let repeated_bytes = bytes.load(Ordering::Relaxed);
            bytes.store(0, Ordering::Relaxed);
            let proof = reader.prove(chain, challenge, limits, &cancelled).unwrap();
            assert!(
                bytes.load(Ordering::Relaxed) < repeated_bytes,
                "proof must avoid repeated chunk reads"
            );
            assert_eq!(
                ProofValidator::new(reader.params().clone())
                    .unwrap()
                    .validate_packed_proof(&proof, challenge),
                Some(chain.fragments)
            );
            assert_eq!(
                reader
                    .prove(chain, challenge, limits, &AtomicBool::new(true))
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::Interrupted
            );
            let mut absent = *chain;
            absent.fragments[0] = u64::MAX;
            assert!(
                reader
                    .prove(&absent, challenge, limits, &cancelled)
                    .unwrap_err()
                    .to_string()
                    .contains("proof fragment absent from plot")
            );
            return;
        }
    }
    panic!("test plot must provide a quality chain");
}
