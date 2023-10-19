use crate::utils::span::Span;

#[derive(Debug, Copy, Clone)]
pub struct ChachaContext {
    pub input: [u32; 16],
}

#[inline]
fn chacha_quarter_round(a: &mut u32, b: &mut u32, c: &mut u32, d: &mut u32) {
    *a = a.wrapping_add(*b);
    *d ^= *a;
    *d = d.rotate_left(16);
    *c = c.wrapping_add(*d);
    *b ^= *c;
    *b = b.rotate_left(12);
    *a = a.wrapping_add(*b);
    *d ^= *a;
    *d = d.rotate_left(8);
    *c = c.wrapping_add(*d);
    *b ^= *c;
    *b = b.rotate_left(7);
}

#[inline]
pub fn chacha8_keysetup(context: &mut ChachaContext, key: &[u8; 32], nonce: Option<&[u8; 8]>) {
    context.input[0] = 0x61707865;
    context.input[1] = 0x3320646E;
    context.input[2] = 0x79622D32;
    context.input[3] = 0x6B206574;
    //Input words 4 through 11 are taken from the 256-bit key, by reading
    //the bytes in little-endian order, in 4-byte chunks
    context.input[4] = from_le_bytes(&key[0..4]);
    context.input[5] = from_le_bytes(&key[4..8]);
    context.input[6] = from_le_bytes(&key[8..12]);
    context.input[7] = from_le_bytes(&key[12..16]);
    context.input[8] = from_le_bytes(&key[16..20]);
    context.input[9] = from_le_bytes(&key[20..24]);
    context.input[10] = from_le_bytes(&key[24..28]);
    context.input[11] = from_le_bytes(&key[28..32]);
    if let Some(nonce) = nonce {
        //Input words 12 and 13 are a block counter, with word 12
        //overflowing into word 13
        context.input[12] = 0;
        context.input[13] = 0;
        //Input words 14 and 15 are taken from an 64-bit nonce, by reading
        //the bytes in little-endian order, in 4-byte chunks
        context.input[14] = from_le_bytes(&nonce[0..4]);
        context.input[15] = from_le_bytes(&nonce[4..8]);
    } else {
        context.input[14] = 0;
        context.input[15] = 0;
    }
}

fn from_le_bytes(buf: impl AsRef<[u8]>) -> u32 {
    let mut out: [u8; 4] = [0; 4];
    out.copy_from_slice(buf.as_ref());
    u32::from_le_bytes(out)
}

pub fn chacha8_get_keystream(
    context: &ChachaContext,
    pos: u64,
    mut n_blocks: u32,
    cypher_text: &mut Vec<u8>,
) {
    let (
        mut x0,
        mut x1,
        mut x2,
        mut x3,
        mut x4,
        mut x5,
        mut x6,
        mut x7,
        mut x8,
        mut x9,
        mut x10,
        mut x11,
        mut x12,
        mut x13,
        mut x14,
        mut x15,
    );
    let j0: u32 = context.input[0];
    let j1: u32 = context.input[1];
    let j2: u32 = context.input[2];
    let j3: u32 = context.input[3];
    let j4: u32 = context.input[4];
    let j5: u32 = context.input[5];
    let j6: u32 = context.input[6];
    let j7: u32 = context.input[7];
    let j8: u32 = context.input[8];
    let j9: u32 = context.input[9];
    let j10: u32 = context.input[10];
    let j11: u32 = context.input[11];
    let mut j12: u32 = pos as u32;
    let mut j13: u32 = (pos >> 32) as u32;
    let j14: u32 = context.input[14];
    let j15: u32 = context.input[15];
    while n_blocks > 0 {
        x0 = j0;
        x1 = j1;
        x2 = j2;
        x3 = j3;
        x4 = j4;
        x5 = j5;
        x6 = j6;
        x7 = j7;
        x8 = j8;
        x9 = j9;
        x10 = j10;
        x11 = j11;
        x12 = j12;
        x13 = j13;
        x14 = j14;
        x15 = j15;
        for _ in 0..4 {
            chacha_quarter_round(&mut x0, &mut x4, &mut x8, &mut x12);
            chacha_quarter_round(&mut x1, &mut x5, &mut x9, &mut x13);
            chacha_quarter_round(&mut x2, &mut x6, &mut x10, &mut x14);
            chacha_quarter_round(&mut x3, &mut x7, &mut x11, &mut x15);
            chacha_quarter_round(&mut x0, &mut x5, &mut x10, &mut x15);
            chacha_quarter_round(&mut x1, &mut x6, &mut x11, &mut x12);
            chacha_quarter_round(&mut x2, &mut x7, &mut x8, &mut x13);
            chacha_quarter_round(&mut x3, &mut x4, &mut x9, &mut x14);
        }
        cypher_text.extend(&x0.wrapping_add(j0).to_le_bytes());
        cypher_text.extend(&x1.wrapping_add(j1).to_le_bytes());
        cypher_text.extend(&x2.wrapping_add(j2).to_le_bytes());
        cypher_text.extend(&x3.wrapping_add(j3).to_le_bytes());
        cypher_text.extend(&x4.wrapping_add(j4).to_le_bytes());
        cypher_text.extend(&x5.wrapping_add(j5).to_le_bytes());
        cypher_text.extend(&x6.wrapping_add(j6).to_le_bytes());
        cypher_text.extend(&x7.wrapping_add(j7).to_le_bytes());
        cypher_text.extend(&x8.wrapping_add(j8).to_le_bytes());
        cypher_text.extend(&x9.wrapping_add(j9).to_le_bytes());
        cypher_text.extend(&x10.wrapping_add(j10).to_le_bytes());
        cypher_text.extend(&x11.wrapping_add(j11).to_le_bytes());
        cypher_text.extend(&x12.wrapping_add(j12).to_le_bytes());
        cypher_text.extend(&x13.wrapping_add(j13).to_le_bytes());
        cypher_text.extend(&x14.wrapping_add(j14).to_le_bytes());
        cypher_text.extend(&x15.wrapping_add(j15).to_le_bytes());
        j12 = j12.wrapping_add(1);
        if j12 == 0 {
            j13 = j13.wrapping_add(1);
        }
        n_blocks -= 1;
    }
}
/// # Safety
///
/// This function assumes a correctly sized array
pub unsafe fn chacha8_get_keystream_unsafe(
    context: &ChachaContext,
    pos: u64,
    mut n_blocks: u32,
    mut cypher_text: Span<u32>,
) {
    let (
        mut x0,
        mut x1,
        mut x2,
        mut x3,
        mut x4,
        mut x5,
        mut x6,
        mut x7,
        mut x8,
        mut x9,
        mut x10,
        mut x11,
        mut x12,
        mut x13,
        mut x14,
        mut x15,
    );

    let j0: u32 = context.input[0];
    let j1: u32 = context.input[1];
    let j2: u32 = context.input[2];
    let j3: u32 = context.input[3];
    let j4: u32 = context.input[4];
    let j5: u32 = context.input[5];
    let j6: u32 = context.input[6];
    let j7: u32 = context.input[7];
    let j8: u32 = context.input[8];
    let j9: u32 = context.input[9];
    let j10: u32 = context.input[10];
    let j11: u32 = context.input[11];
    let mut j12: u32 = pos as u32;
    let mut j13: u32 = (pos >> 32) as u32;
    let j14: u32 = context.input[14];
    let j15: u32 = context.input[15];
    let mut output_index = 0isize;
    while n_blocks > 0 {
        x0 = j0;
        x1 = j1;
        x2 = j2;
        x3 = j3;
        x4 = j4;
        x5 = j5;
        x6 = j6;
        x7 = j7;
        x8 = j8;
        x9 = j9;
        x10 = j10;
        x11 = j11;
        x12 = j12;
        x13 = j13;
        x14 = j14;
        x15 = j15;
        for _ in 0..4 {
            chacha_quarter_round(&mut x0, &mut x4, &mut x8, &mut x12);
            chacha_quarter_round(&mut x1, &mut x5, &mut x9, &mut x13);
            chacha_quarter_round(&mut x2, &mut x6, &mut x10, &mut x14);
            chacha_quarter_round(&mut x3, &mut x7, &mut x11, &mut x15);
            chacha_quarter_round(&mut x0, &mut x5, &mut x10, &mut x15);
            chacha_quarter_round(&mut x1, &mut x6, &mut x11, &mut x12);
            chacha_quarter_round(&mut x2, &mut x7, &mut x8, &mut x13);
            chacha_quarter_round(&mut x3, &mut x4, &mut x9, &mut x14);
        }
        cypher_text[output_index] = x0.wrapping_add(j0).to_le();
        cypher_text[output_index + 1] = x1.wrapping_add(j1).to_le();
        cypher_text[output_index + 2] = x2.wrapping_add(j2).to_le();
        cypher_text[output_index + 3] = x3.wrapping_add(j3).to_le();
        cypher_text[output_index + 4] = x4.wrapping_add(j4).to_le();
        cypher_text[output_index + 5] = x5.wrapping_add(j5).to_le();
        cypher_text[output_index + 6] = x6.wrapping_add(j6).to_le();
        cypher_text[output_index + 7] = x7.wrapping_add(j7).to_le();
        cypher_text[output_index + 8] = x8.wrapping_add(j8).to_le();
        cypher_text[output_index + 9] = x9.wrapping_add(j9).to_le();
        cypher_text[output_index + 10] = x10.wrapping_add(j10).to_le();
        cypher_text[output_index + 11] = x11.wrapping_add(j11).to_le();
        cypher_text[output_index + 12] = x12.wrapping_add(j12).to_le();
        cypher_text[output_index + 13] = x13.wrapping_add(j13).to_le();
        cypher_text[output_index + 14] = x14.wrapping_add(j14).to_le();
        cypher_text[output_index + 15] = x15.wrapping_add(j15).to_le();
        j12 = j12.wrapping_add(1);
        if j12 == 0 {
            j13 = j13.wrapping_add(1);
        }
        output_index += 16;
        n_blocks -= 1;
    }
}
