use bech32::{FromBase32, ToBase32, Variant};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_puzzles::programs;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use flate2::read::ZlibEncoder;
use flate2::{Compress, Compression, Decompress, FlushDecompress, Status};
use std::io::{Error, Read};
use std::sync::LazyLock;

const MAX_DECOMPRESSED_BYTES: u64 = 6 * 1024 * 1024;
static DICTIONARY: LazyLock<Vec<u8>> = LazyLock::new(|| {
    [
        programs::P2_DELEGATED_PUZZLE_OR_HIDDEN_PUZZLE_SRC,
        dg_xch_puzzles::cats::CAT_1_SRC,
        dg_xch_puzzles::settlement_payments::SETTLEMENT_PAYMENTS_V1_SRC,
        programs::SINGLETON_TOP_LAYER_V1_1_SRC,
        programs::NFT_STATE_LAYER_SRC,
        programs::NFT_OWNERSHIP_LAYER_SRC,
        programs::NFT_METADATA_UPDATER_DEFAULT_SRC,
        programs::NFT_OWNERSHIP_TRANSFER_PROGRAM_ONE_WAY_CLAIM_WITH_ROYALTIES_SRC,
        programs::CAT_PUZZLE_SRC,
        programs::SETTLEMENT_PAYMENT_SRC,
    ]
    .concat()
});

pub(super) fn encode(bundle: &SpendBundle) -> Result<String, Error> {
    let bytes = bundle.to_bytes(ChiaProtocolVersion::default())?;
    if bytes.len() as u64 > MAX_DECOMPRESSED_BYTES {
        return Err(Error::other("offer exceeds decompressed size limit"));
    }
    let mut compressor = Compress::new(Compression::new(6), true);
    compressor
        .set_dictionary(&DICTIONARY)
        .map_err(Error::other)?;
    let mut compressed = 6u16.to_be_bytes().to_vec();
    ZlibEncoder::new_with_compress(bytes.as_slice(), compressor).read_to_end(&mut compressed)?;
    let text =
        bech32::encode("offer", compressed.to_base32(), Variant::Bech32m).map_err(Error::other)?;
    if text.len() > super::MAX_OFFER_BYTES {
        return Err(Error::other("offer exceeds 1 MiB"));
    }
    Ok(text)
}

pub(super) fn decode(text: &str) -> Result<SpendBundle, Error> {
    if text.len() > super::MAX_OFFER_BYTES {
        return Err(Error::other("offer exceeds 1 MiB"));
    }
    let (prefix, data, variant) = bech32::decode(text.trim()).map_err(Error::other)?;
    if prefix != "offer" || variant != Variant::Bech32m {
        return Err(Error::other("expected bech32m offer"));
    }
    let bytes = Vec::<u8>::from_base32(&data).map_err(Error::other)?;
    let version = bytes
        .get(..2)
        .ok_or_else(|| Error::other("missing offer version"))?;
    if u16::from_be_bytes([version[0], version[1]]) > 6 {
        return Err(Error::other("unsupported offer version"));
    }
    let input = &bytes[2..];
    let mut decompressor = Decompress::new(true);
    let mut output = Vec::new();
    let mut dictionary_set = false;
    loop {
        let before_in = decompressor.total_in();
        let before_out = decompressor.total_out();
        let mut chunk = [0u8; 8192];
        let status = decompressor.decompress(
            &input[before_in as usize..],
            &mut chunk,
            FlushDecompress::None,
        );
        match status {
            Err(error) if error.needs_dictionary().is_some() && !dictionary_set => {
                decompressor
                    .set_dictionary(&DICTIONARY)
                    .map_err(Error::other)?;
                dictionary_set = true;
                continue;
            }
            Err(error) => return Err(Error::other(error)),
            Ok(status) => {
                if decompressor.total_out() > MAX_DECOMPRESSED_BYTES {
                    return Err(Error::other("offer exceeds decompressed size limit"));
                }
                output
                    .extend_from_slice(&chunk[..(decompressor.total_out() - before_out) as usize]);
                if status == Status::StreamEnd {
                    if decompressor.total_in() as usize != input.len() {
                        return Err(Error::other("trailing offer compression data"));
                    }
                    break;
                }
                if decompressor.total_in() == before_in && decompressor.total_out() == before_out {
                    return Err(Error::other("truncated offer compression stream"));
                }
            }
        }
    }
    SpendBundle::from_bytes_exact(&output, ChiaProtocolVersion::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_offer_encoding_matches_reference() {
        let native = SpendBundle::default();
        let reference = "offer1qqr83wcuu2rykcmqvpsrsqpzdqyqqjryqrqsl3uv6v";
        assert_eq!(encode(&native).unwrap(), reference);
        assert_eq!(decode(reference).unwrap(), native);
        assert!(decode("offer1invalid").is_err());
        let wrong_prefix = reference.replacen("offer", "xch", 1);
        assert!(decode(&wrong_prefix).is_err());
        let (_, data, _) = bech32::decode(reference).unwrap();
        let compressed = Vec::<u8>::from_base32(&data).unwrap();
        for length in 0..compressed.len() {
            let truncated = bech32::encode(
                "offer",
                (&compressed[..length]).to_base32(),
                Variant::Bech32m,
            )
            .unwrap();
            assert!(
                decode(&truncated).is_err(),
                "accepted truncated stream at {length}"
            );
        }
        let mut trailing = compressed.clone();
        trailing.push(0);
        let text = bech32::encode("offer", trailing.to_base32(), Variant::Bech32m).unwrap();
        assert!(decode(&text).is_err());
    }
}
