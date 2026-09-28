use crate::chacha8::{ChachaContext, chacha8_get_keystream, chacha8_keysetup};
use crate::constants::{K_C3R, K_CHECKPOINT1INTERVAL};
use crate::encoding::ans_decode_deltas;
use crate::verifier::validate_proof;
use dg_xch_core::utils::hash_256;
use std::io::{Error, ErrorKind, Read, Seek, SeekFrom};

const MAGIC: &[u8; 19] = b"Proof of Space Plot";
const FORMAT: &[u8; 8] = b"mmx-v3.0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gh3Header {
    pub plot_id: [u8; 32],
    pub compression_level: u8,
    pub compression_parameters: [u8; 3],
    pub encoding_parameters: [u8; 14],
    pub table_offsets: [u64; 5],
    pub header_size: u64,
    pub file_size: u64,
    encrypted_memo: Vec<u8>,
}

impl Gh3Header {
    pub fn read(reader: &mut (impl Read + Seek)) -> Result<Self, Error> {
        let file_size = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        if &read_array::<19>(reader)? != MAGIC {
            return Err(invalid("Invalid GH 3.0 plot magic"));
        }
        let plot_id = read_array(reader)?;
        if read_array::<1>(reader)? != [32] {
            return Err(invalid("GH 3.0 requires K32"));
        }
        if u16::from_be_bytes(read_array(reader)?) != FORMAT.len() as u16
            || &read_array::<8>(reader)? != FORMAT
        {
            return Err(invalid("Unsupported GH plot format; expected mmx-v3.0"));
        }
        let memo_size = usize::from(u16::from_be_bytes(read_array(reader)?));
        if !(1..=4096).contains(&memo_size) {
            return Err(invalid("Invalid GH 3.0 encrypted memo length"));
        }
        let mut encrypted_memo = vec![0; memo_size];
        reader.read_exact(&mut encrypted_memo)?;
        let compression_parameters: [u8; 3] = read_array(reader)?;
        let compression_level = if compression_parameters[0] == 0 {
            38_u8.checked_sub(compression_parameters[1])
        } else {
            35_u8.checked_sub(compression_parameters[0])
        }
        .filter(|level| (29..=33).contains(level))
        .ok_or_else(|| invalid("Unsupported GH 3.0 compression parameters"))?;
        let encoding_parameters = read_array(reader)?;
        let mut table_offsets = [0; 5];
        for offset in &mut table_offsets {
            *offset = u64::from_be_bytes(read_array(reader)?);
        }
        let header_size = reader.stream_position()?;
        if table_offsets[0] < header_size
            || table_offsets.iter().any(|offset| *offset >= file_size)
            || table_offsets.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(invalid("Invalid GH 3.0 table offsets"));
        }
        Ok(Self {
            plot_id,
            compression_level,
            compression_parameters,
            encoding_parameters,
            table_offsets,
            header_size,
            file_size,
            encrypted_memo,
        })
    }

    #[must_use]
    pub fn encrypted_memo(&self) -> &[u8] {
        &self.encrypted_memo
    }

    pub fn decrypt_og_memo(&self) -> Result<Gh3OgMemo, Error> {
        if self.compression_level != 30 || self.encrypted_memo.len() != 128 {
            return Err(invalid(
                "Memo recovery currently supports only GH 3.0 C30 OG plots",
            ));
        }
        let stream = memo_stream(&self.plot_id);
        let mut plaintext = [0; 128];
        for ((output, encrypted), mask) in
            plaintext.iter_mut().zip(&self.encrypted_memo).zip(stream)
        {
            *output = encrypted ^ mask;
        }
        Ok(Gh3OgMemo {
            pool_public_key: plaintext[..48].try_into().unwrap(),
            farmer_public_key: plaintext[48..96].try_into().unwrap(),
            local_master_secret_key: plaintext[96..].try_into().unwrap(),
        })
    }

    pub fn verify_proof(
        &self,
        challenge: &[u8; 32],
        proof: &[u8],
        expected_quality: &[u8; 32],
    ) -> Result<(), Error> {
        let quality = validate_proof(&self.plot_id, 32, proof, challenge)?;
        if quality != (*expected_quality).into() {
            return Err(invalid(
                "GH proof quality does not match the expected quality",
            ));
        }
        Ok(())
    }
}

pub struct Gh3OgMemo {
    pub pool_public_key: [u8; 48],
    pub farmer_public_key: [u8; 48],
    pub local_master_secret_key: [u8; 32],
}

fn memo_stream(plot_id: &[u8; 32]) -> [u8; 128] {
    let mut seed = [0_u8; 62];
    for (index, byte) in seed.iter_mut().enumerate() {
        *byte = u8::try_from(index).unwrap().wrapping_mul(201);
    }
    seed[14..46].copy_from_slice(plot_id);
    let key = hash_256(seed);
    let mut context = ChachaContext { input: [0; 16] };
    chacha8_keysetup(&mut context, &key, None);
    let mut stream = Vec::with_capacity(192);
    chacha8_get_keystream(&context, 29_489, 3, &mut stream);
    stream[1..129].try_into().unwrap()
}

pub struct Gh3Reader<Reader> {
    reader: Reader,
    header: Gh3Header,
    checkpoint_count: u64,
}

impl<Reader: Read + Seek> Gh3Reader<Reader> {
    pub fn new(mut reader: Reader) -> Result<Self, Error> {
        let header = Gh3Header::read(&mut reader)?;
        let checkpoint_bytes = header.table_offsets[3] - header.table_offsets[2];
        if checkpoint_bytes < 8 || checkpoint_bytes % 4 != 0 {
            return Err(invalid("Invalid GH 3.0 C1 table size"));
        }
        let checkpoint_count = checkpoint_bytes / 4 - 1;
        let mut result = Self {
            reader,
            header,
            checkpoint_count,
        };
        if result.checkpoint(checkpoint_count)? != 0 {
            return Err(invalid("Missing GH 3.0 C1 terminator"));
        }
        Ok(result)
    }

    #[must_use]
    pub fn header(&self) -> &Gh3Header {
        &self.header
    }

    pub fn matching_f7_indices(&mut self, challenge: &[u8; 32]) -> Result<Vec<u64>, Error> {
        let target = u32::from_be_bytes(challenge[..4].try_into().unwrap());
        let mut lower = 0;
        let mut upper = self.checkpoint_count;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            if self.checkpoint(middle)? < target {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        let mut park = lower.saturating_sub(1);
        let mut matches = Vec::new();
        while park < self.checkpoint_count {
            let base = self.checkpoint(park)?;
            if base > target {
                break;
            }
            for (position, value) in self.read_f7_park(park, base)?.into_iter().enumerate() {
                if value == target {
                    matches.push(park * u64::from(K_CHECKPOINT1INTERVAL) + position as u64);
                }
            }
            if matches.len() > 65536 {
                return Err(invalid("Excessive GH 3.0 F7 matches"));
            }
            park += 1;
        }
        Ok(matches)
    }

    fn checkpoint(&mut self, index: u64) -> Result<u32, Error> {
        if index > self.checkpoint_count {
            return Err(invalid("GH 3.0 checkpoint index out of range"));
        }
        self.reader
            .seek(SeekFrom::Start(self.header.table_offsets[2] + index * 4))?;
        Ok(u32::from_be_bytes(read_array(&mut self.reader)?))
    }

    pub fn c30_bucket_indices(&mut self, f7_index: u64) -> Result<Vec<u64>, Error> {
        self.c30_selected_bucket_indices(f7_index, None)
    }

    pub fn c30_quality_bucket_indices(
        &mut self,
        f7_index: u64,
        challenge: &[u8; 32],
    ) -> Result<Vec<u64>, Error> {
        self.c30_selected_bucket_indices(f7_index, Some((challenge[31] >> 4) & 1))
    }

    fn c30_selected_bucket_indices(
        &mut self,
        f7_index: u64,
        quality_side: Option<u8>,
    ) -> Result<Vec<u64>, Error> {
        if self.header.compression_parameters != [0, 8, 11] {
            return Err(invalid(
                "CPU reconstruction requires C30 parameters [0, 8, 11]",
            ));
        }
        let park_size = u64::from(u32::from_le_bytes(
            self.header.encoding_parameters[4..8].try_into().unwrap(),
        ));
        let address = self.header.table_offsets[1]
            .checked_add(
                (f7_index / 16384)
                    .checked_mul(park_size)
                    .ok_or_else(|| invalid("T7 offset overflow"))?,
            )
            .ok_or_else(|| invalid("T7 offset overflow"))?;
        let park = self.read_region(address, park_size as usize, self.header.table_offsets[2])?;
        let bit = (f7_index % 16384) as usize * 42;
        let encoded = read_bits(&park, bit, 42)?;
        let point = encoded >> 1;
        if point == 0 {
            return Err(invalid("Invalid C30 line point"));
        }
        let mut lower = 1_u64;
        let mut upper = 1_u64 << 22;
        while lower + 1 < upper {
            let middle = (lower + upper) / 2;
            if middle * (middle - 1) / 2 <= point {
                lower = middle;
            } else {
                upper = middle;
            }
        }
        let remainder = point - lower * (lower - 1) / 2;
        let (first, second) = if remainder == 0 {
            (lower - 2, lower - 2)
        } else {
            (lower - 1, remainder - 1)
        };
        let flags_offset = 42 * 16384 / 8;
        let compressed_size = u16::from_be_bytes(
            park.get(flags_offset..flags_offset + 2)
                .ok_or_else(|| invalid("Truncated T7 flags"))?
                .try_into()
                .unwrap(),
        ) as usize;
        let flags = if compressed_size == 0 {
            vec![0; 32768]
        } else {
            let payload = park
                .get(flags_offset + 2..flags_offset + 2 + compressed_size)
                .ok_or_else(|| invalid("Invalid T7 flags size"))?;
            decode_binary(
                payload,
                32768,
                u16::from_le_bytes(self.header.encoding_parameters[12..14].try_into().unwrap()),
            )?
        };
        let mut indices = Vec::with_capacity(4);
        let position = (f7_index % 16384) as usize * 2;
        if quality_side.is_none_or(|side| u64::from(side) != encoded & 1) {
            indices.push(first);
            if flags[position] != 0 {
                indices.push(first + 1);
            }
        }
        if quality_side.is_none_or(|side| u64::from(side) == encoded & 1) {
            indices.push(second);
            if flags[position + 1] != 0 {
                indices.push(second + 1);
            }
        }
        indices.sort_unstable();
        indices.dedup();
        if indices.iter().any(|index| *index >= 1 << 21) {
            return Err(invalid("Invalid C30 bucket index"));
        }
        Ok(indices)
    }

    pub fn c30_bitmap(&mut self, index: u64) -> Result<Vec<u64>, Error> {
        if self.header.compression_parameters != [0, 8, 11] || index >= 1 << 21 {
            return Err(invalid("Invalid C30 bitmap request"));
        }
        let base = self.header.table_offsets[0];
        let limit = self.header.table_offsets[1];
        let prefix = self.read_region(base, 8, limit)?;
        let sizes_offset = u64::from_be_bytes(prefix.try_into().unwrap());
        if sizes_offset > limit - base {
            return Err(invalid("Invalid bitmap sizes offset"));
        }
        let group = self.read_region(base + 8 + (index / 1024) * 8, 8, limit)?;
        let mut offset = u64::from_be_bytes(group.try_into().unwrap());
        if offset > limit - base {
            return Err(invalid("Invalid bitmap group offset"));
        }
        let sizes = self.read_region(
            base + sizes_offset + (index / 1024) * 2048,
            (index as usize % 1024 + 1) * 2,
            limit,
        )?;
        let mut length = 0;
        for pair in sizes.as_chunks::<2>().0 {
            offset += length;
            length = u64::from(u16::from_le_bytes(*pair) ^ 0x1337);
        }
        let max_size = u32::from_le_bytes(self.header.encoding_parameters[..4].try_into().unwrap());
        if length == 0 || length > u64::from(max_size) {
            return Err(invalid("Invalid C30 bitmap length"));
        }
        let payload = self.read_region(base + offset, length as usize, limit)?;
        let bits = decode_binary(
            &payload,
            1 << 19,
            u16::from_le_bytes(self.header.encoding_parameters[10..12].try_into().unwrap()),
        )?;
        let mut bitmap = vec![0_u64; 1 << 13];
        for (position, bit) in bits.into_iter().enumerate() {
            bitmap[position / 64] |= u64::from(bit) << (position % 64);
        }
        Ok(bitmap)
    }

    fn read_region(&mut self, address: u64, length: usize, limit: u64) -> Result<Vec<u8>, Error> {
        if length > 1 << 20
            || address
                .checked_add(length as u64)
                .is_none_or(|end| end > limit)
        {
            return Err(invalid("GH region lies outside its table"));
        }
        self.reader.seek(SeekFrom::Start(address))?;
        let mut bytes = vec![0; length];
        self.reader.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    fn read_f7_park(&mut self, index: u64, base: u32) -> Result<Vec<u32>, Error> {
        let park_size = u64::from(u16::from_le_bytes(
            self.header.encoding_parameters[8..10].try_into().unwrap(),
        ));
        if park_size < 2 {
            return Err(invalid("Invalid GH 3.0 C3 park size"));
        }
        let address = self.header.table_offsets[4]
            .checked_add(
                index
                    .checked_mul(park_size)
                    .ok_or_else(|| invalid("C3 offset overflow"))?,
            )
            .ok_or_else(|| invalid("C3 offset overflow"))?;
        if address == self.header.file_size && index + 1 == self.checkpoint_count {
            return Ok(vec![base]);
        }
        if address > self.header.file_size || self.header.file_size - address < 2 {
            return Err(invalid("GH 3.0 C3 park lies outside the file"));
        }
        self.reader.seek(SeekFrom::Start(address))?;
        let compressed_size = usize::from(u16::from_be_bytes(read_array(&mut self.reader)?));
        if compressed_size == 0 {
            if index + 1 != self.checkpoint_count {
                return Err(invalid("Empty nonfinal GH 3.0 C3 park"));
            }
            return Ok(vec![base]);
        }
        if compressed_size as u64 > park_size - 2
            || compressed_size as u64 > self.header.file_size - address - 2
        {
            return Err(invalid("Invalid GH 3.0 C3 payload length"));
        }
        let mut compressed = vec![0; compressed_size];
        self.reader.read_exact(&mut compressed)?;
        let (count, deltas) = ans_decode_deltas(
            &compressed,
            compressed_size,
            K_CHECKPOINT1INTERVAL as usize,
            K_C3R,
        )?;
        if count >= K_CHECKPOINT1INTERVAL as usize || count > deltas.len() {
            return Err(invalid("Invalid GH 3.0 C3 delta count"));
        }
        let mut values = Vec::with_capacity(count + 1);
        values.push(base);
        let mut previous = base;
        for delta in &deltas[..count] {
            previous = previous
                .checked_add(u32::from(*delta))
                .ok_or_else(|| invalid("GH 3.0 F7 overflow"))?;
            values.push(previous);
        }
        Ok(values)
    }
}

#[must_use]
pub fn challenge_for(counter: u64) -> [u8; 32] {
    let mut seed = [0; 24];
    seed[..14].copy_from_slice(b"dg-gh3-diag-v1");
    seed[16..].copy_from_slice(&counter.to_le_bytes());
    hash_256(seed)
}

fn transport_key(plot_id: &[u8; 32], salt: u64) -> [u8; 32] {
    let mut seed = [0; 41];
    seed[..8].copy_from_slice(&salt.to_le_bytes());
    seed[8..40].copy_from_slice(plot_id);
    hash_256(seed)
}

#[must_use]
pub fn encrypt_challenge(plot_id: &[u8; 32], challenge: &[u8; 32], salt: u64) -> [u8; 32] {
    let mut encrypted = transport_key(plot_id, salt);
    for (byte, challenge_byte) in encrypted.iter_mut().zip(challenge) {
        *byte ^= challenge_byte;
    }
    encrypted
}

#[must_use]
pub fn decrypt_signature(plot_id: &[u8; 32], signature: &[u8; 96], salt: u64) -> [u8; 96] {
    let mut context = ChachaContext { input: [0; 16] };
    chacha8_keysetup(&mut context, &transport_key(plot_id, salt), None);
    let mut stream = Vec::with_capacity(128);
    chacha8_get_keystream(&context, 0, 2, &mut stream);
    let mut decrypted = *signature;
    for (byte, mask) in decrypted.iter_mut().zip(stream.iter().skip(1)) {
        *byte ^= mask;
    }
    decrypted
}

fn read_array<const SIZE: usize>(reader: &mut impl Read) -> Result<[u8; SIZE], Error> {
    let mut bytes = [0; SIZE];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

fn read_bits(bytes: &[u8], start: usize, count: usize) -> Result<u64, Error> {
    if count > 64
        || start
            .checked_add(count)
            .is_none_or(|end| end > bytes.len() * 8)
    {
        return Err(invalid("Invalid packed bit range"));
    }
    let mut value = 0;
    for bit in start..start + count {
        value = (value << 1) | u64::from((bytes[bit / 8] >> (7 - bit % 8)) & 1);
    }
    Ok(value)
}

fn decode_binary(input: &[u8], size: usize, frequency: u16) -> Result<Vec<u8>, Error> {
    use crate::finite_state_entropy::decompress::{build_dtable, decompress_using_dtable};
    use std::sync::Arc;
    if frequency == 0 || frequency >= 16384 {
        return Err(invalid("Invalid binary FSE frequency"));
    }
    let table = Arc::new(build_dtable(
        &[(16384 - frequency) as i16, frequency as i16],
        1,
        14,
    )?);
    let mut decoded = vec![0; size];
    let count = decompress_using_dtable(&mut decoded, size, input, input.len(), table)?;
    if count != size || decoded.iter().any(|value| *value > 1) {
        return Err(invalid("Invalid binary FSE output"));
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn header_bytes() -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&[7; 32]);
        bytes.push(32);
        bytes.extend_from_slice(&8_u16.to_be_bytes());
        bytes.extend_from_slice(FORMAT);
        bytes.extend_from_slice(&128_u16.to_be_bytes());
        bytes.extend_from_slice(&[9; 128]);
        bytes.extend_from_slice(&[0, 8, 11]);
        bytes.extend_from_slice(&[0; 14]);
        for offset in [256_u64, 512, 768, 1024, 1280] {
            bytes.extend_from_slice(&offset.to_be_bytes());
        }
        bytes.resize(1536, 0);
        bytes
    }

    #[test]
    fn reads_opaque_memo_and_compression() {
        let header = Gh3Header::read(&mut Cursor::new(header_bytes())).unwrap();
        assert_eq!(header.plot_id, [7; 32]);
        assert_eq!(header.compression_level, 30);
        assert_eq!(header.header_size, 249);
        assert_eq!(header.encrypted_memo(), &[9; 128]);
    }

    #[test]
    fn challenge_seed_is_stable() {
        assert_eq!(
            hex::encode(challenge_for(0)),
            "2bf7e386a39727dbe96f68be56425ed920c5d585bba606c2e73642602a24eee6"
        );
    }

    #[test]
    fn rejects_unverified_memo_variants() {
        let mut header = Gh3Header::read(&mut Cursor::new(header_bytes())).unwrap();
        header.compression_level = 29;
        assert!(header.decrypt_og_memo().is_err());
        header.compression_level = 30;
        header.encrypted_memo.resize(112, 0);
        assert!(header.decrypt_og_memo().is_err());
    }

    fn checkpoint_fixture() -> Vec<u8> {
        let deltas = [1, 0, 1, 3].repeat(30);
        let compressed = crate::encoding::ans_encode_deltas(&deltas, K_C3R).unwrap();
        let park_size = u16::try_from(compressed.len() + 2).unwrap();
        let mut bytes = header_bytes();
        bytes[203..205].copy_from_slice(&park_size.to_le_bytes());
        for (index, offset) in [256_u64, 512, 768, 776, 784].into_iter().enumerate() {
            bytes[209 + index * 8..217 + index * 8].copy_from_slice(&offset.to_be_bytes());
        }
        bytes[768..772].copy_from_slice(&100_u32.to_be_bytes());
        bytes[772..776].fill(0);
        bytes[784..786].copy_from_slice(&(compressed.len() as u16).to_be_bytes());
        bytes[786..786 + compressed.len()].copy_from_slice(&compressed);
        bytes.truncate(786 + compressed.len());
        bytes
    }

    #[test]
    fn decodes_c30_bucket_pairs_and_rejects_bad_offsets() {
        let mut bytes = header_bytes();
        bytes.resize(88536, 0);
        bytes[199..203].copy_from_slice(&88006_u32.to_le_bytes());
        for (index, offset) in [256_u64, 512, 88518, 88526, 88534].into_iter().enumerate() {
            bytes[209 + index * 8..217 + index * 8].copy_from_slice(&offset.to_be_bytes());
        }
        let packed = ((101_u64 * 100 / 2 + 18) << 1) << 6;
        bytes[512..518].copy_from_slice(&packed.to_be_bytes()[2..]);
        let mut reader = Gh3Reader::new(Cursor::new(bytes.clone())).unwrap();
        assert_eq!(reader.c30_bucket_indices(0).unwrap(), [17, 100]);
        assert_eq!(
            reader.c30_quality_bucket_indices(0, &[0; 32]).unwrap(),
            [17]
        );
        assert_eq!(
            reader.c30_quality_bucket_indices(0, &[16; 32]).unwrap(),
            [100]
        );
        bytes[517] |= 64;
        let mut reversed = Gh3Reader::new(Cursor::new(bytes.clone())).unwrap();
        assert_eq!(
            reversed.c30_quality_bucket_indices(0, &[0; 32]).unwrap(),
            [100]
        );
        assert_eq!(
            reversed.c30_quality_bucket_indices(0, &[16; 32]).unwrap(),
            [17]
        );
        assert!(reader.c30_bucket_indices(u64::MAX).is_err());
        bytes[512..518].fill(0);
        assert!(
            Gh3Reader::new(Cursor::new(bytes.clone()))
                .unwrap()
                .c30_bucket_indices(0)
                .is_err()
        );
        bytes[256..264].copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(
            Gh3Reader::new(Cursor::new(bytes))
                .unwrap()
                .c30_bitmap(0)
                .is_err()
        );
    }

    #[test]
    fn finds_duplicate_f7_values_in_compressed_checkpoint() {
        let mut reader = Gh3Reader::new(Cursor::new(checkpoint_fixture())).unwrap();
        let mut challenge = [0; 32];
        challenge[..4].copy_from_slice(&101_u32.to_be_bytes());
        assert_eq!(reader.matching_f7_indices(&challenge).unwrap(), [1, 2]);
        challenge[..4].copy_from_slice(&99_u32.to_be_bytes());
        assert!(reader.matching_f7_indices(&challenge).unwrap().is_empty());
        challenge[..4].copy_from_slice(&100_u32.to_be_bytes());
        assert_eq!(reader.matching_f7_indices(&challenge).unwrap(), [0]);
    }

    #[test]
    fn rejects_checkpoint_payload_outside_park() {
        let mut bytes = checkpoint_fixture();
        bytes[784..786].copy_from_slice(&u16::MAX.to_be_bytes());
        let mut reader = Gh3Reader::new(Cursor::new(bytes)).unwrap();
        let mut challenge = [0; 32];
        challenge[..4].copy_from_slice(&101_u32.to_be_bytes());
        assert!(reader.matching_f7_indices(&challenge).is_err());
    }
}
