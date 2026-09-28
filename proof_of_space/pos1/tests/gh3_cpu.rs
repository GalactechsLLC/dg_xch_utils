use dg_xch_pos1::gigahorse::{Gh3Reader, challenge_for};
use dg_xch_pos1::gigahorse_cpu::{finish_c30_proofs, reconstruct_c30_table5};
use dg_xch_pos1::verifier::validate_proof;
use std::fs::File;

#[test]
#[ignore = "requires GH_C30_PLOT and several GiB of RAM; run with --release"]
fn reconstructs_real_c30_proof() {
    let path = std::env::var_os("GH_C30_PLOT").expect("Set GH_C30_PLOT");
    let mut reader = Gh3Reader::new(File::open(path).unwrap()).unwrap();
    let challenge = challenge_for(1);
    let indices = reader.matching_f7_indices(&challenge).unwrap();
    let index = *indices
        .first()
        .expect("plot must have a proof for challenge 1");
    let mut entries = Vec::new();
    for bucket in reader.c30_bucket_indices(index).unwrap() {
        let bitmap = reader.c30_bitmap(bucket).unwrap();
        entries.extend(reconstruct_c30_table5(&reader.header().plot_id, &bitmap).unwrap());
    }
    let proofs = finish_c30_proofs(entries, &challenge);
    assert!(!proofs.is_empty());
    for proof in proofs {
        validate_proof(&reader.header().plot_id, 32, &proof, &challenge).unwrap();
    }
}
