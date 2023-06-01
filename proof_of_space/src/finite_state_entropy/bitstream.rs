use std::io::{Error, ErrorKind};
use std::mem::size_of;

pub enum BitDstreamStatus {
    Unfinished = 0,
    EndOfBuffer = 1,
    Completed = 2,
    Overflow = 3,
}
impl BitDstreamStatus {
    pub const fn eq(&self, other: Self) -> bool {
        matches!(
            (self, other),
            (BitDstreamStatus::Unfinished, BitDstreamStatus::Unfinished)
                | (BitDstreamStatus::EndOfBuffer, BitDstreamStatus::EndOfBuffer)
                | (BitDstreamStatus::Completed, BitDstreamStatus::Completed)
                | (BitDstreamStatus::Overflow, BitDstreamStatus::Overflow)
        )
    }
    pub const fn gt(self, other: Self) -> bool {
        (self as u8) > (other as u8)
    }
}

pub const fn highbit_32(u: u32) -> u32 {
    if u == 0 {
        return 0;
    }
    u.ilog2()
}

const BIT_MASK: [u32; 32] = [
    0, 1, 3, 7, 0xF, 0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF, 0x1FFF, 0x3FFF, 0x7FFF,
    0xFFFF, 0x1FFFF, 0x3FFFF, 0x7FFFF, 0xFFFFF, 0x1FFFFF, 0x3FFFFF, 0x7FFFFF, 0xFFFFFF, 0x1FFFFFF,
    0x3FFFFFF, 0x7FFFFFF, 0xFFFFFFF, 0x1FFFFFFF, 0x3FFFFFFF, 0x7FFFFFFF,
]; /* up to 31 bits */

pub struct BitDstream<'a> {
    pub bit_container: usize,
    pub index: usize,
    pub limit: usize,
    src: &'a [u8],
    bits_consumed: u32,
}
impl<'a> BitDstream<'a> {
    pub fn new(src: &'a [u8], src_size: usize) -> Result<Self, Error> {
        if src_size < 1 {
            return Err(Error::new(ErrorKind::InvalidInput, "src_size wrong"));
        }
        let mut bit_container = 0;
        let mut bits_consumed;
        let mut index = 0;
        let limit = size_of::<usize>();
        if src_size >= size_of::<usize>() {
            /* normal case */
            index = src_size - size_of::<usize>();
            bit_container = Self::safe_create_usize(&src[index..index + size_of::<usize>()]);
            let last_byte = src[src_size - 1];
            bits_consumed = if last_byte > 0 {
                8 - highbit_32(last_byte as u32)
            } else {
                0
            };
            if last_byte == 0 {
                return Err(Error::new(ErrorKind::InvalidInput, "end mark not present"));
            } /* endMark not present */
        } else {
            //Is this needed?
            bit_container += src[0] as usize;
            match src_size {
                7 => {
                    bit_container += (src[6] as usize) << (usize::BITS - 16);
                    bit_container += (src[5] as usize) << (usize::BITS - 24);
                    bit_container += (src[4] as usize) << (usize::BITS - 32);
                    bit_container += (src[3] as usize) << 24;
                    bit_container += (src[2] as usize) << 16;
                    bit_container += (src[1] as usize) << 8;
                }
                6 => {
                    bit_container += (src[5] as usize) << (usize::BITS - 24);
                    bit_container += (src[4] as usize) << (usize::BITS - 32);
                    bit_container += (src[3] as usize) << 24;
                    bit_container += (src[2] as usize) << 16;
                    bit_container += (src[1] as usize) << 8;
                }
                5 => {
                    bit_container += (src[4] as usize) << (usize::BITS - 32);
                    bit_container += (src[3] as usize) << 24;
                    bit_container += (src[2] as usize) << 16;
                    bit_container += (src[1] as usize) << 8;
                }
                4 => {
                    bit_container += (src[3] as usize) << 24;
                    bit_container += (src[2] as usize) << 16;
                    bit_container += (src[1] as usize) << 8;
                }
                3 => {
                    bit_container += (src[2] as usize) << 16;
                    bit_container += (src[1] as usize) << 8;
                }
                2 => {
                    bit_container += (src[1] as usize) << 8;
                }
                _ => {}
            }
            let last_byte = src[src_size - 1];
            bits_consumed = if last_byte > 0 {
                8 - highbit_32(last_byte as u32)
            } else {
                0
            };
            if last_byte == 0 {
                return Err(Error::new(ErrorKind::InvalidInput, "end mark not present"));
            } /* endMark not present */
            bits_consumed += ((size_of::<usize>() - src_size) * 8) as u32;
        }
        Ok(BitDstream {
            bit_container,
            index,
            limit,
            src,
            bits_consumed,
        })
    }
    pub fn reload(&mut self) -> BitDstreamStatus {
        if self.bits_consumed > usize::BITS {
            /* overflow detected, like end of stream */
            return BitDstreamStatus::Overflow;
        }
        if self.index >= self.limit {
            return self.reload_fast();
        }
        if self.index == 0 {
            if self.bits_consumed < usize::BITS {
                return BitDstreamStatus::EndOfBuffer;
            }
            return BitDstreamStatus::Completed;
        }
        let mut nb_bytes = self.bits_consumed >> 3;
        let mut result = BitDstreamStatus::Unfinished;
        if self.index < (nb_bytes as usize) {
            nb_bytes = self.index as u32; /* ptr > start */
            result = BitDstreamStatus::EndOfBuffer;
        }
        self.index -= nb_bytes as usize;
        self.bits_consumed -= nb_bytes * 8;
        self.bit_container =
            Self::safe_create_usize(&self.src[self.index..self.index + size_of::<usize>()]);
        result
    }

    fn reload_fast(&mut self) -> BitDstreamStatus {
        self.index -= self.bits_consumed as usize >> 3;
        self.bits_consumed &= 7;
        self.bit_container =
            Self::safe_create_usize(&self.src[self.index..self.index + size_of::<usize>()]);
        BitDstreamStatus::Unfinished
    }

    const fn safe_create_usize(buf: &[u8]) -> usize {
        let mut sized: [u8; size_of::<usize>()] = [0; size_of::<usize>()];
        let mut i = 0;
        while i < buf.len() && i < size_of::<usize>() {
            sized[i] = buf[i];
            i += 1;
        }
        usize::from_le_bytes(sized)
    }

    /* bit_look_bits() :
     *  Provides next n bits from local register.
     *  local register is not modified.
     *  On 32-bits, maxNbBits==24.
     *  On 64-bits, maxNbBits==56.
     * @return : value extracted */
    pub fn bit_look_bits(&mut self, nb_bits: u32) -> usize {
        get_middle_bits(
            self.bit_container,
            usize::BITS - self.bits_consumed - nb_bits,
            nb_bits,
        )
    }

    /* bit_look_bits_fast() :
     *  unsafe version; only works if nbBits >= 1 */
    pub fn bit_look_bits_fast(&mut self, nb_bits: u32) -> usize {
        let reg_mask = usize::BITS - 1;
        (self.bit_container << (self.bits_consumed & reg_mask))
            >> (((reg_mask + 1) - nb_bits) & reg_mask)
    }

    pub fn skip_bits(&mut self, nb_bits: u32) {
        self.bits_consumed += nb_bits;
    }

    pub fn read_bits(&mut self, nb_bits: u32) -> usize {
        let value: usize = self.bit_look_bits(nb_bits);
        self.skip_bits(nb_bits);
        value
    }

    pub fn read_bits_fast(&mut self, nb_bits: u32) -> usize {
        let value: usize = self.bit_look_bits_fast(nb_bits);
        self.skip_bits(nb_bits);
        value
    }
}

pub const fn get_middle_bits(bit_container: usize, start: u32, nb_bits: u32) -> usize {
    let reg_mask = usize::BITS - 1;
    (bit_container >> (start & reg_mask)) & BIT_MASK[nb_bits as usize] as usize
}
