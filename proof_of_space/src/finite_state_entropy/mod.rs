//Ported to Rust, based on the original C version here: https://github.com/Cyan4973/FiniteStateEntropy

mod bitstream;
pub mod compress;
pub mod decompress;

use crate::finite_state_entropy::compress::CTable;
use crate::finite_state_entropy::decompress::DTable;
use std::io::{Error, ErrorKind};
use std::mem::size_of;

pub const FSE_VERSION_MAJOR: u8 = 0;
pub const FSE_VERSION_MINOR: u8 = 9;
pub const FSE_VERSION_RELEASE: u8 = 0;
pub const FSE_TABLELOG_ABSOLUTE_MAX: i32 = 15;
pub const FSE_NCOUNTBOUND: usize = 512;
pub const FSE_MAX_MEMORY_USAGE: u32 = 16;
pub const FSE_DEFAULT_MEMORY_USAGE: usize = 16;
pub const FSE_MAX_TABLELOG: u32 = FSE_MAX_MEMORY_USAGE - 2;
pub const FSE_MAX_TABLESIZE: usize = (1u32 << FSE_MAX_TABLELOG) as usize;
pub const FSE_MAXTABLESIZE_MASK: usize = FSE_MAX_TABLESIZE - 1;
pub const FSE_DEFAULT_TABLELOG: usize = FSE_DEFAULT_MEMORY_USAGE - 2;
pub const FSE_MIN_TABLELOG: u32 = 5;

// const fn fse_blockbound(size: usize) -> usize {size + (size>>7) + 4 + size_of::<usize>()}
// const fn fse_compressbound(size: usize) -> usize {FSE_NCOUNTBOUND + fse_blockbound(size)}
//
#[must_use]
pub const fn fse_ctable_size_u32(max_table_log: u32, max_symbol_value: u32) -> u32 {
    1 + (1 << ((max_table_log) - 1)) + (((max_symbol_value) + 1) * 2)
}
#[must_use]
pub const fn fse_dtable_size_u32(max_table_log: u32) -> u32 {
    1 + (1 << max_table_log)
}
#[allow(clippy::cast_possible_truncation)]
#[must_use]
pub const fn fse_ctable_size(max_table_log: u32, max_symbol_value: u32) -> u32 {
    fse_ctable_size_u32(max_table_log, max_symbol_value) * size_of::<CTable>() as u32
}
#[allow(clippy::cast_possible_truncation)]
#[must_use]
pub const fn fse_dtable_size(max_table_log: u32) -> u32 {
    fse_dtable_size_u32(max_table_log) * size_of::<DTable>() as u32
}
//
// const fn fse_wksp_size_u32(max_table_log: u32, max_symbol_value: u32) -> u32 {fse_ctable_size_u32(max_table_log, max_symbol_value) + if max_table_log > 12 { 1 << (max_table_log - 2) } else { 1024 }}

#[must_use]
pub const fn fse_tablestep(table_size: u32) -> u32 {
    (table_size >> 1) + (table_size >> 3) + 3
}

// pub fn compress(src: Vec<u8>) -> Result<Vec<u8>, Error> {
//     //Todo
//     Ok(vec![])
// }

/*-**************************************************************
*  FSE NCount encoding-decoding
****************************************************************/
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::too_many_lines)]
#[allow(clippy::cast_sign_loss)]
#[allow(clippy::cast_possible_wrap)]
pub fn read_ncount(
    normalized_counter: &mut [i16],
    max_symbol_value: &mut u32,
    table_log: &mut u32,
    src: &[u8],
) -> Result<usize, Error> {
    let mut index = 0;
    if src.len() < 4 {
        /* This function only works when hbSize >= 4 */
        let mut buffer = vec![];
        buffer.extend(src);
        while buffer.len() < 4 {
            buffer.push(0u8);
        }
        let count_size = read_ncount(normalized_counter, max_symbol_value, table_log, &buffer)?;
        if count_size > src.len() {
            return Err(Error::new(ErrorKind::InvalidInput, "corruption detected"));
        }
        return Ok(count_size);
    }
    //assert(hbSize >= 4); //Todo convert to Error
    normalized_counter.fill(0); //memset(normalized_counter, 0, (*max_svptr +1) * sizeof(normalized_counter[0]));   /* all symbols not present in NCount have a frequency of 0 */
    let mut bit_stream: u32 = u32::from_le_bytes(
        src[index..index + size_of::<u32>()]
            .try_into()
            .map_err(|e| {
                Error::new(ErrorKind::InvalidInput, format!("Should Not Happen: {e:?}"))
            })?,
    );
    let mut nb_bits: i32 = ((bit_stream & 0xF) + FSE_MIN_TABLELOG) as i32; /* extract tableLog */
    if nb_bits > FSE_TABLELOG_ABSOLUTE_MAX {
        return Err(Error::new(ErrorKind::InvalidInput, "table log too large"));
    }
    bit_stream >>= 4;
    let mut bit_count = 4;
    *table_log = nb_bits as u32;
    let mut remaining = (1 << nb_bits) + 1;
    let mut threshold = 1 << nb_bits;
    nb_bits += 1;
    let mut charnum: u32 = 0;
    let mut previous0 = false;
    while (remaining > 1) & (charnum <= *max_symbol_value) {
        if previous0 {
            let mut n0: u32 = charnum;
            while bit_stream & 0xFFFF == 0xFFFF {
                n0 += 24;
                if index < src.len() - 5 {
                    index += 2;
                    bit_stream = u32::from_le_bytes(
                        src[index..index + size_of::<u32>()]
                            .try_into()
                            .map_err(|e| {
                                Error::new(
                                    ErrorKind::InvalidInput,
                                    format!("Should Not Happen: {e:?}"),
                                )
                            })?,
                    ) >> bit_count;
                } else {
                    bit_stream >>= 16;
                    bit_count += 16;
                }
            }
            while bit_stream & 3 == 3 {
                n0 += 3;
                bit_stream >>= 2;
                bit_count += 2;
            }
            n0 += bit_stream & 3;
            bit_count += 2;
            if n0 > *max_symbol_value {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "max symbol value too small",
                ));
            }
            while charnum < n0 {
                normalized_counter[charnum as usize] = 0;
                charnum += 1;
            }
            if index <= src.len() - 7 || index + (bit_count >> 3) <= src.len() - 4 {
                //assert((bit_count >> 3) <= 3); /* For first condition to work *///Todo convert to Error
                index += bit_count >> 3;
                bit_count &= 7;
                bit_stream =
                    u32::from_le_bytes(src[index..index + size_of::<u32>()].try_into().map_err(
                        |e| {
                            Error::new(ErrorKind::InvalidInput, format!("Should Not Happen: {e:?}"))
                        },
                    )?) >> bit_count;
            } else {
                bit_stream >>= 2;
            }
        }
        let max = (2 * threshold - 1) - remaining;
        let mut count: i32;
        if bit_stream & (threshold - 1) < max {
            count = (bit_stream & (threshold - 1)) as i32;
            bit_count += (nb_bits - 1) as usize;
        } else {
            count = (bit_stream & (2 * threshold - 1)) as i32;
            if count >= threshold as i32 {
                count -= max as i32;
            }
            bit_count += nb_bits as usize;
        }
        count -= 1; /* extra accuracy */
        remaining -= if count < 0 { -count } else { count } as u32; /* -1 means +1 */
        normalized_counter[charnum as usize] = count as i16;
        charnum += 1;
        previous0 = count == 0;
        while remaining < threshold {
            nb_bits -= 1;
            threshold >>= 1;
        }
        if index <= src.len() - 7 || index + (bit_count >> 3) <= src.len() - 4 {
            index += bit_count >> 3;
            bit_count &= 7;
        } else {
            bit_count -= 8 * (src.len() - 4 - index);
            index = src.len() - 4;
        }
        bit_stream = u32::from_le_bytes(src[index..index + size_of::<u32>()].try_into().map_err(
            |e| Error::new(ErrorKind::InvalidInput, format!("Should Not Happen: {e:?}")),
        )?) >> (bit_count & 31);
    }
    if remaining != 1 {
        return Err(Error::new(ErrorKind::InvalidInput, "corruption detected"));
    }
    if bit_count > 32 {
        return Err(Error::new(ErrorKind::InvalidInput, "corruption detected"));
    }
    *max_symbol_value = charnum - 1;
    index += (bit_count + 7) >> 3;
    Ok(index)
}
