use blst::min_pk::SecretKey;
use dg_xch_plotter::format::{PackedChunk, write_packed_chunks};
use dg_xch_plotter::{PlotInfo, PlotRequest, PoolBinding, create_with_writer, read_metadata};
use std::io::ErrorKind;
use std::path::Path;
use std::sync::atomic::AtomicBool;

fn empty_plot(path: &Path, portable: bool) -> (PlotInfo, Vec<u8>) {
    let farmer = SecretKey::key_gen_v3(&[7; 32], &[])
        .unwrap()
        .sk_to_pk()
        .to_bytes();
    let pool = SecretKey::key_gen_v3(&[8; 32], &[])
        .unwrap()
        .sk_to_pk()
        .to_bytes();
    let request = PlotRequest {
        farmer_public_key: farmer,
        pool: if portable {
            PoolBinding::Contract([9; 32])
        } else {
            PoolBinding::PublicKey(pool)
        },
        k: 28,
        strength: 3,
        index: 511,
        meta_group: 5,
        testnet: false,
    };
    let cancelled = AtomicBool::new(false);
    let mut expected_memo = Vec::new();
    let info = create_with_writer(&request, path, &cancelled, |params, output, memo| {
        expected_memo.extend_from_slice(memo);
        write_packed_chunks(
            output,
            &params,
            1,
            request.index,
            request.meta_group,
            memo,
            &cancelled,
            |_| {
                Ok(PackedChunk {
                    count: 0,
                    deltas: Vec::new(),
                    stubs: Vec::new(),
                })
            },
        )
    })
    .unwrap();
    (info, expected_memo)
}

#[test]
fn metadata_returns_validated_header_and_exact_memo_for_both_pool_bindings() {
    for portable in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty.plot");
        let (expected_info, expected_memo) = empty_plot(&path, portable);
        let metadata = read_metadata(&path).unwrap();
        assert_eq!(metadata.info, expected_info);
        assert_eq!(metadata.memo.as_slice(), expected_memo.as_slice());
        assert_eq!(metadata.memo.len(), if portable { 112 } else { 128 });
        assert_eq!(metadata.info.portable, portable);
        assert_eq!(metadata.info.k, 28);
        assert_eq!(metadata.info.strength, 3);
        assert_eq!(metadata.info.index, 511);
        assert_eq!(metadata.info.meta_group, 5);
        assert_eq!(metadata.info.chunks, 1);
        assert_eq!(
            metadata.info.file_bytes,
            (43 + expected_memo.len() + 8 + 8 + 20) as u64
        );
        assert!(SecretKey::from_bytes(&metadata.memo[metadata.memo.len() - 32..]).is_ok());
    }
}

#[test]
fn metadata_rejects_corrupt_plot_identity_and_valid_but_mismatched_memos() {
    for portable in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity.plot");
        empty_plot(&path, portable);
        let original = std::fs::read(&path).unwrap();
        for offset in [5, 39, 41] {
            let mut corrupt = original.clone();
            corrupt[offset] ^= 1;
            std::fs::write(&path, corrupt).unwrap();
            assert_eq!(
                read_metadata(&path).err().unwrap().kind(),
                ErrorKind::InvalidData
            );
        }
        let mut wrong_memo = original.clone();
        if portable {
            wrong_memo[43] ^= 1;
        } else {
            let different_pool = SecretKey::key_gen_v3(&[10; 32], &[])
                .unwrap()
                .sk_to_pk()
                .to_bytes();
            wrong_memo[43..43 + 48].copy_from_slice(&different_pool);
        }
        std::fs::write(&path, wrong_memo).unwrap();
        assert_eq!(
            read_metadata(&path).err().unwrap().kind(),
            ErrorKind::InvalidData
        );
        std::fs::write(&path, original).unwrap();
        assert!(read_metadata(&path).is_ok());
    }
}

#[test]
fn metadata_rejects_truncated_header_memo_directory_and_chunk() {
    for portable in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("truncated.plot");
        let (_, memo) = empty_plot(&path, portable);
        let original = std::fs::read(&path).unwrap();
        let memo_end = 43 + memo.len();
        for length in [
            0,
            4,
            42,
            43,
            memo_end - 1,
            memo_end,
            memo_end + 7,
            memo_end + 8,
            memo_end + 15,
            original.len() - 1,
        ] {
            std::fs::write(&path, &original[..length]).unwrap();
            let error = read_metadata(&path).err().unwrap();
            assert!(
                matches!(
                    error.kind(),
                    ErrorKind::UnexpectedEof | ErrorKind::InvalidData
                ),
                "length {length}: {error}"
            );
        }
    }
}
