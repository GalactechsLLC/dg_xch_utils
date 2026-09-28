use crate::finite_state_entropy::FSE_MAX_SYMBOL_VALUE;
use crate::finite_state_entropy::bitstream::{BitCStream, highbit_32};
use crate::finite_state_entropy::{FSE_MAX_TABLELOG, fse_tablestep};
use std::io::{Error, ErrorKind};

/// Per-symbol encoding transform.
///
/// `delta_nb_bits` is a deliberately wrapped `u32`: the reference implementation stores
/// `(max_bits_out << 16) - min_state_plus` in unsigned arithmetic and relies on the wrap, so that
/// `state + delta_nb_bits` later shifts down to the right output bit count.
#[derive(Default, Clone, Copy)]
pub struct SymbolTransform {
    pub delta_nb_bits: u32,
    pub delta_find_state: i32,
}

/// The encoding counterpart of `DTable`. Built from the same normalized counts, so a stream
/// produced here decodes with `build_dtable` for the same `(counts, max_symbol_value, table_log)`.
#[derive(Default, Clone)]
pub struct CTable {
    pub table_log: u32,
    pub max_symbol_value: u32,
    pub state_table: Vec<u16>,
    pub symbol_tt: Vec<SymbolTransform>,
}

#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_sign_loss)]
#[allow(clippy::cast_possible_wrap)]
pub fn build_ctable(
    normalized_counter: &[i16],
    max_symbol_value: u32,
    table_log: u32,
) -> Result<CTable, Error> {
    if max_symbol_value > FSE_MAX_SYMBOL_VALUE {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "max_symbol_value too large",
        ));
    }
    if table_log > FSE_MAX_TABLELOG {
        return Err(Error::new(ErrorKind::InvalidInput, "table_log too large"));
    }
    let max_sv1 = (max_symbol_value + 1) as usize;
    if normalized_counter.len() < max_sv1 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "normalized_counter shorter than max_symbol_value",
        ));
    }
    let table_size = 1usize << table_log;
    let table_mask = table_size - 1;
    let step = fse_tablestep(table_size as u32) as usize;

    // Symbol start positions. A -1 count is a low probability symbol: it gets one slot, taken from
    // the top of the table rather than from the spread below.
    let mut cumul = vec![0u32; max_sv1 + 1];
    let mut table_symbol = vec![0u8; table_size];
    let mut high_threshold = table_size as i64 - 1;
    for u in 1..=max_sv1 {
        if normalized_counter[u - 1] == -1 {
            cumul[u] = cumul[u - 1] + 1;
            table_symbol[high_threshold as usize] = (u - 1) as u8;
            high_threshold -= 1;
        } else {
            cumul[u] = cumul[u - 1] + normalized_counter[u - 1] as u32;
        }
    }
    cumul[max_sv1] = table_size as u32 + 1;

    // Spread symbols over the table.
    let mut position = 0usize;
    for (symbol, count) in normalized_counter.iter().enumerate().take(max_sv1) {
        for _ in 0..(*count).max(0) {
            table_symbol[position] = symbol as u8;
            position = (position + step) & table_mask;
            while position as i64 > high_threshold {
                position = (position + step) & table_mask;
            }
        }
    }
    if position != 0 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "normalized_counter is incorrect",
        ));
    }

    // Next-state table, sorted by symbol.
    let mut state_table = vec![0u16; table_size];
    for (u, symbol) in table_symbol.iter().enumerate() {
        let s = *symbol as usize;
        state_table[cumul[s] as usize] = (table_size + u) as u16;
        cumul[s] += 1;
    }

    let mut symbol_tt = vec![SymbolTransform::default(); max_sv1];
    let mut total: i32 = 0;
    for (s, count) in normalized_counter.iter().enumerate().take(max_sv1) {
        match *count {
            0 => {
                symbol_tt[s].delta_nb_bits =
                    ((table_log + 1) << 16).wrapping_sub(1u32 << table_log);
            }
            -1 | 1 => {
                symbol_tt[s].delta_nb_bits = (table_log << 16).wrapping_sub(1u32 << table_log);
                symbol_tt[s].delta_find_state = total - 1;
                total += 1;
            }
            n => {
                let max_bits_out = table_log - highbit_32((n - 1) as u32);
                let min_state_plus = (n as u32) << max_bits_out;
                symbol_tt[s].delta_nb_bits = (max_bits_out << 16).wrapping_sub(min_state_plus);
                symbol_tt[s].delta_find_state = total - i32::from(n);
                total += i32::from(n);
            }
        }
    }

    Ok(CTable {
        table_log,
        max_symbol_value,
        state_table,
        symbol_tt,
    })
}

#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_sign_loss)]
fn init_cstate2(ct: &CTable, symbol: u8) -> usize {
    let tt = ct.symbol_tt[symbol as usize];
    let nb_bits_out = tt.delta_nb_bits.wrapping_add(1 << 15) >> 16;
    let value = (nb_bits_out << 16).wrapping_sub(tt.delta_nb_bits);
    let index = (value >> nb_bits_out) as i32 + tt.delta_find_state;
    ct.state_table[index as usize] as usize
}

#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_sign_loss)]
fn encode_symbol(bit_c: &mut BitCStream, state: &mut usize, ct: &CTable, symbol: u8) {
    let tt = ct.symbol_tt[symbol as usize];
    let nb_bits_out = (*state as u32).wrapping_add(tt.delta_nb_bits) >> 16;
    bit_c.add_bits(*state, nb_bits_out);
    let index = ((*state as u32) >> nb_bits_out) as i32 + tt.delta_find_state;
    *state = ct.state_table[index as usize] as usize;
}

/// Encode `src` with `ct`. The stream is written back to front and carries two interleaved states,
/// which is what lets the decoder walk it forwards.
///
/// Returns an empty stream for inputs of two symbols or fewer, matching the reference: such an
/// input cannot pay for its own two state flushes, and the caller stores it uncompressed instead.
pub fn compress_using_ctable(src: &[u8], ct: &CTable) -> Result<Vec<u8>, Error> {
    if ct.state_table.is_empty() {
        return Err(Error::new(ErrorKind::InvalidInput, "ctable is empty"));
    }
    if src.len() <= 2 {
        return Ok(Vec::new());
    }
    let mut bit_c = BitCStream::new();
    let mut ip = src.len();

    let (mut state1, mut state2);
    if src.len() & 1 == 1 {
        ip -= 1;
        state1 = init_cstate2(ct, src[ip]);
        ip -= 1;
        state2 = init_cstate2(ct, src[ip]);
        ip -= 1;
        encode_symbol(&mut bit_c, &mut state1, ct, src[ip]);
        bit_c.flush_bits();
    } else {
        ip -= 1;
        state2 = init_cstate2(ct, src[ip]);
        ip -= 1;
        state1 = init_cstate2(ct, src[ip]);
    }

    // Bring the remaining count to a multiple of four so the main loop can encode four symbols
    // between flushes. A 64 bit register holds 4 * 14 + 7 bits, so four is the most it can carry.
    let joined = src.len() - 2;
    if usize::BITS > FSE_MAX_TABLELOG * 4 + 7 && joined & 2 != 0 {
        ip -= 1;
        encode_symbol(&mut bit_c, &mut state2, ct, src[ip]);
        ip -= 1;
        encode_symbol(&mut bit_c, &mut state1, ct, src[ip]);
        bit_c.flush_bits();
    }

    while ip > 0 {
        ip -= 1;
        encode_symbol(&mut bit_c, &mut state2, ct, src[ip]);
        if usize::BITS < FSE_MAX_TABLELOG * 2 + 7 {
            bit_c.flush_bits();
        }
        ip -= 1;
        encode_symbol(&mut bit_c, &mut state1, ct, src[ip]);
        if usize::BITS > FSE_MAX_TABLELOG * 4 + 7 {
            ip -= 1;
            encode_symbol(&mut bit_c, &mut state2, ct, src[ip]);
            ip -= 1;
            encode_symbol(&mut bit_c, &mut state1, ct, src[ip]);
        }
        bit_c.flush_bits();
    }

    bit_c.add_bits(state2, ct.table_log);
    bit_c.flush_bits();
    bit_c.add_bits(state1, ct.table_log);
    bit_c.flush_bits();
    Ok(bit_c.close())
}
