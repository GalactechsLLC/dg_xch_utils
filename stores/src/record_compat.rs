use crate::error::StoreError;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::sized_bytes::Bytes100;
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::{Cursor, Error, ErrorKind};

const VERSION: ChiaProtocolVersion = ChiaProtocolVersion::Chia0_0_37;

/// Decode a stored record blob: wire layout first (exact fit), legacy layout as the fallback.
pub(crate) fn decode_record(blob: &[u8]) -> Result<BlockRecord, StoreError> {
    let mut cur = Cursor::new(blob);
    let chia_err = match BlockRecord::from_bytes(&mut cur, VERSION) {
        Ok(rec) if cur.position() == blob.len() as u64 => return Ok(rec),
        Ok(_) => Error::new(
            ErrorKind::InvalidData,
            "trailing bytes after chia-layout block record",
        ),
        Err(e) => e,
    };
    // Not a wire-layout blob — try the legacy stored form. Surface the wire-layout error if the
    // legacy walk fails too: a blob that parses as neither is corrupt, and the current-layout
    // diagnosis is the useful one.
    decode_legacy_record(blob).map_err(|_| StoreError::Io(chia_err))
}

/// The legacy layout: identical to the wire layout except the two VDF outputs are
/// length-prefixed byte vectors instead of bare 100-byte values.
fn decode_legacy_record(blob: &[u8]) -> Result<BlockRecord, Error> {
    fn f<T: ChiaSerialize>(c: &mut Cursor<&[u8]>) -> Result<T, Error> {
        T::from_bytes(c, VERSION)
    }
    fn legacy_vdf(c: &mut Cursor<&[u8]>) -> Result<ClassgroupElement, Error> {
        let data = UnsizedBytes::from_bytes(c, VERSION)?;
        let arr: [u8; 100] = data.as_slice().try_into().map_err(|_| {
            Error::new(ErrorKind::InvalidData, "legacy VDF output is not 100 bytes")
        })?;
        Ok(ClassgroupElement {
            data: Bytes100::from(arr),
        })
    }
    let mut c = Cursor::new(blob);
    let record = BlockRecord {
        header_hash: f(&mut c)?,
        prev_hash: f(&mut c)?,
        height: f(&mut c)?,
        weight: f(&mut c)?,
        total_iters: f(&mut c)?,
        signage_point_index: f(&mut c)?,
        challenge_vdf_output: legacy_vdf(&mut c)?,
        infused_challenge_vdf_output: match u8::from_bytes(&mut c, VERSION)? {
            0 => None,
            1 => Some(legacy_vdf(&mut c)?),
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "invalid Option tag in legacy-layout block record",
                ));
            }
        },
        reward_infusion_new_challenge: f(&mut c)?,
        challenge_block_info_hash: f(&mut c)?,
        sub_slot_iters: f(&mut c)?,
        pool_puzzle_hash: f(&mut c)?,
        farmer_puzzle_hash: f(&mut c)?,
        required_iters: f(&mut c)?,
        deficit: f(&mut c)?,
        overflow: f(&mut c)?,
        prev_transaction_block_height: f(&mut c)?,
        timestamp: f(&mut c)?,
        prev_transaction_block_hash: f(&mut c)?,
        fees: f(&mut c)?,
        reward_claims_incorporated: f(&mut c)?,
        finished_challenge_slot_hashes: f(&mut c)?,
        finished_infused_challenge_slot_hashes: f(&mut c)?,
        finished_reward_slot_hashes: f(&mut c)?,
        sub_epoch_summary_included: f(&mut c)?,
    };
    if c.position() != blob.len() as u64 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "trailing bytes after legacy-layout block record",
        ));
    }
    Ok(record)
}

#[cfg(test)]
#[path = "../tests/unit/record_compat/tests.rs"]
mod tests;
