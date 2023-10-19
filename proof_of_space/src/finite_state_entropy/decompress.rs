use crate::constants::FSE_MAX_SYMBOL_VALUE;
use crate::finite_state_entropy::bitstream::{highbit_32, BitDstream, BitDstreamStatus};
use crate::finite_state_entropy::{
    fse_dtable_size_u32, fse_tablestep, FSE_MAX_TABLELOG, FSE_TABLELOG_ABSOLUTE_MAX,
};
use std::io::{Cursor, Error, ErrorKind, Read};
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct DTableH {
    pub table_log: u16,
    pub fast_mode: u16,
}

#[derive(Default, Clone)]
pub struct DTableEntry {
    pub new_state: u16,
    pub symbol: u8,
    pub nb_bits: u8,
}

#[derive(Default, Clone)]
pub struct DTable {
    pub header: DTableH,
    pub table: Vec<DTableEntry>,
}
pub fn parse_d_table(bytes: &[u8]) -> DTable {
    let mut cursor = Cursor::new(bytes);
    let mut u16_buf: [u8; 2] = [0; 2];
    let mut u8_buf: [u8; 1] = [0; 1];
    cursor.read_exact(&mut u16_buf).unwrap();
    let table_log = u16::from_le_bytes(u16_buf);
    cursor.read_exact(&mut u16_buf).unwrap();
    let fast_mode = u16::from_le_bytes(u16_buf);
    let mut table = vec![];
    let max_size = fse_dtable_size_u32(table_log as u32);
    for _ in 0..max_size {
        let new_state = match cursor.read_exact(&mut u16_buf) {
            Ok(_) => u16::from_le_bytes(u16_buf),
            Err(_) => 0,
        };
        let symbol = match cursor.read_exact(&mut u8_buf) {
            Ok(_) => u8::from_le_bytes(u8_buf),
            Err(_) => 0,
        };
        let nb_bits = match cursor.read_exact(&mut u8_buf) {
            Ok(_) => u8::from_le_bytes(u8_buf),
            Err(_) => 0,
        };
        table.push(DTableEntry {
            new_state,
            symbol,
            nb_bits,
        });
    }
    DTable {
        header: DTableH {
            table_log,
            fast_mode,
        },
        table,
    }
}

pub struct DState {
    pub state: usize,
    pub table: Arc<DTable>,
}
impl DState {
    pub fn new(bit_d: &mut BitDstream, dt: Arc<DTable>) -> Self {
        let state = bit_d.read_bits(dt.header.table_log as u32);
        bit_d.reload();
        DState { state, table: dt }
    }
}

fn create_dtable(table_log: u32) -> DTable {
    let size = if table_log > FSE_TABLELOG_ABSOLUTE_MAX as u32 {
        fse_dtable_size_u32(FSE_TABLELOG_ABSOLUTE_MAX as u32)
    } else {
        fse_dtable_size_u32(table_log)
    } as usize;
    DTable {
        header: DTableH {
            table_log: 0,
            fast_mode: 0,
        },
        table: vec![DTableEntry::default(); size],
    }
}

pub fn build_dtable(
    normalized_counter: &[i16],
    max_symbol_value: u32,
    table_log: u32,
) -> Result<DTable, Error> {
    let mut dt = create_dtable(table_log);
    let mut symbol_next = vec![0u16; (FSE_MAX_SYMBOL_VALUE + 1) as usize];
    let max_sv1 = max_symbol_value + 1;
    let table_size = 1 << table_log;

    /* Sanity Checks */
    if max_symbol_value > FSE_MAX_SYMBOL_VALUE {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "max_symbol_value too large",
        ));
    }
    if table_log > FSE_MAX_TABLELOG {
        return Err(Error::new(ErrorKind::InvalidInput, "table_log too large"));
    }

    /* Init, lay down lowprob symbols */
    dt.header.table_log = table_log as u16;
    dt.header.fast_mode = 1;
    let large_limit = (1 << (table_log - 1)) as i16;
    let mut high_threshold = table_size - 1;
    for (index, (normalize, symbol_next)) in normalized_counter
        .iter()
        .zip(symbol_next.iter_mut())
        .enumerate()
        .take(max_sv1 as usize)
    {
        if *normalize == -1 {
            dt.table[high_threshold as usize].symbol = index as u8;
            high_threshold -= 1;
            *symbol_next = 1;
        } else {
            if *normalize >= large_limit {
                dt.header.fast_mode = 0;
            }
            *symbol_next = *normalize as u16;
        }
    }
    /* Spread symbols */
    let table_mask = table_size - 1;
    let step = fse_tablestep(table_size);
    let mut position: u32 = 0;
    for s in 0..max_sv1 {
        for _ in 0..normalized_counter[s as usize] {
            dt.table[position as usize].symbol = s as u8;
            position = (position + step) & table_mask;
            while position > high_threshold {
                /* lowprob area */
                position = (position + step) & table_mask;
            }
        }
    }
    if position != 0 {
        /* position must reach all cells once, otherwise normalizedCounter is incorrect */
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "normalized_counter is incorrect",
        ));
    }
    /* Build Decoding table */
    for table in &mut dt.table[0..(table_size as usize)] {
        let next_state = symbol_next[table.symbol as usize];
        symbol_next[table.symbol as usize] += 1;
        table.nb_bits = (table_log - highbit_32(next_state as u32)) as u8;
        table.new_state = ((next_state << table.nb_bits) as u32 - table_size) as u16;
    }
    Ok(dt)
}

pub fn decompress_using_dtable(
    mut dst: impl AsMut<[u8]>,
    dst_size: usize,
    src: impl AsRef<[u8]>,
    src_size: usize,
    dt: Arc<DTable>,
) -> Result<usize, Error> {
    let fast = dt.header.fast_mode > 0;
    fse_decompress_using_dtable_generic(dst.as_mut(), dst_size, src.as_ref(), src_size, dt, fast)
}

trait SymbolFn {
    fn decode_symbol(&self, state: &mut DState, bit_d: &mut BitDstream) -> u8;
}

pub fn fse_decompress_using_dtable_generic(
    dst: &mut [u8],
    dst_size: usize,
    src: &[u8],
    src_size: usize,
    dt: Arc<DTable>,
    fast: bool,
) -> Result<usize, Error> {
    let mut bit_d = match BitDstream::new(src, src_size) {
        Ok(b) => b,
        Err(e) => {
            return Err(e);
        }
    };
    /* Init */
    let mut index = 0;
    let limit = dst_size - 3;
    let mut state1 = DState::new(&mut bit_d, dt.clone());
    let mut state2 = DState::new(&mut bit_d, dt);
    let symbol_fn: Box<dyn SymbolFn> = if fast {
        Box::new(FastDecodeSymbol {})
    } else {
        Box::new(DecodeSymbol {})
    };
    /* 4 symbols per loop */
    while bit_d.reload().eq(BitDstreamStatus::Unfinished) & (index < limit) {
        dst[index] = symbol_fn.decode_symbol(&mut state1, &mut bit_d);
        if FSE_MAX_TABLELOG * 2 + 7 > usize::BITS {
            bit_d.reload();
        }
        dst[index + 1] = symbol_fn.decode_symbol(&mut state2, &mut bit_d);
        if FSE_MAX_TABLELOG * 4 + 7 > usize::BITS && bit_d.reload().gt(BitDstreamStatus::Unfinished)
        {
            index += 2;
            break;
        }
        dst[index + 2] = symbol_fn.decode_symbol(&mut state1, &mut bit_d);
        if FSE_MAX_TABLELOG * 2 + 7 > usize::BITS {
            bit_d.reload();
        }
        dst[index + 3] = symbol_fn.decode_symbol(&mut state2, &mut bit_d);
        index += 4;
    }
    loop {
        if index > dst_size - 2 {
            return Err(Error::new(ErrorKind::InvalidInput, "dst_size too small"));
        }
        dst[index] = symbol_fn.decode_symbol(&mut state1, &mut bit_d);
        index += 1;
        if bit_d.reload().eq(BitDstreamStatus::Overflow) {
            dst[index] = symbol_fn.decode_symbol(&mut state2, &mut bit_d);
            break;
        }
        if index > dst_size - 2 {
            return Err(Error::new(ErrorKind::InvalidInput, "dst_size too small"));
        }
        dst[index] = symbol_fn.decode_symbol(&mut state2, &mut bit_d);
        index += 1;
        if bit_d.reload().eq(BitDstreamStatus::Overflow) {
            dst[index] = symbol_fn.decode_symbol(&mut state1, &mut bit_d);
            break;
        }
    }
    Ok(index)
}

pub struct DecodeSymbol {}
impl SymbolFn for DecodeSymbol {
    fn decode_symbol(&self, state: &mut DState, bit_d: &mut BitDstream) -> u8 {
        let entry = &state.table.table[state.state];
        let low_bits: usize = bit_d.read_bits(entry.nb_bits as u32);
        state.state = entry.new_state as usize + low_bits;
        entry.symbol
    }
}

// FSE_decodeSymbolFast():unsafe, only works if no symbol has a probability > 50%
pub struct FastDecodeSymbol {}
impl SymbolFn for FastDecodeSymbol {
    fn decode_symbol(&self, state: &mut DState, bit_d: &mut BitDstream) -> u8 {
        let entry = &state.table.table[state.state];
        let low_bits: usize = bit_d.read_bits_fast(entry.nb_bits as u32);
        state.state = entry.new_state as usize + low_bits;
        entry.symbol
    }
}
