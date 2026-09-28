use crate::clvm::program::Program;
use crate::clvm::sexp::SExp;
use crate::clvm::sexp_ext::SExpNumber;
use crate::errors::ClvmError;
use crate::formatting::prep_hex_str;
use crate::traits::SizedBytes;
use bytes::Buf;
use const_hex::const_decode_to_array;
use dg_xch_serialize::ChiaProtocolVersion;
use dg_xch_serialize::ChiaSerialize;
use hex::encode;
use log::warn;
use num_traits::AsPrimitive;
use rand::{Fill, Rng};
use secrecy::zeroize::DefaultIsZeroes;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cmp::min;
use std::io::{Cursor, Error, ErrorKind, Read};
use std::ops::{
    Add, BitAnd, BitOr, BitXor, Deref, DerefMut, Index, IndexMut, Range, Shl, ShlAssign, Shr,
    ShrAssign,
};
use std::str::FromStr;

#[derive(Copy, Clone)]
pub struct SizedBytesImpl<const SIZE: usize> {
    bytes: [u8; SIZE],
}
impl<const SIZE: usize> SizedBytesImpl<SIZE> {
    pub const fn const_new(bytes: [u8; SIZE]) -> Self {
        Self { bytes }
    }
    pub const fn const_bytes(&self) -> [u8; SIZE] {
        self.bytes
    }
}
impl<const SIZE: usize> Deref for SizedBytesImpl<SIZE> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}
impl<const SIZE: usize> DerefMut for SizedBytesImpl<SIZE> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.bytes
    }
}
macro_rules! impl_const_sized_bytes {
    ($($n:literal),+ $(,)?) => {$(
        impl SizedBytesImpl<$n>  {
            #[track_caller]
            pub const fn const_hex(hex: &str) -> Self {
                let bytes = hex.as_bytes();
                let has_prefix = bytes.len() >= 2
                    && bytes[0] == b'0'
                    && (bytes[1] == b'x' || bytes[1] == b'X');
                let modifier = has_prefix as usize * 2;
                if bytes.len() != $n * 2 + modifier{
                    panic!(concat!("hex length is wrong for Bytes", stringify!($n)));
                }
                match const_decode_to_array::<$n>(bytes) {
                    Ok(bytes) => Self { bytes },
                    Err(_) => panic!(concat!("invalid hex for Bytes", stringify!($n))),
                }
            }
            pub const fn const_bitand(self, rhs: SizedBytesImpl<$n>) -> SizedBytesImpl<$n> {
                let mut out = [0u8; $n];
                let mut i = 0;
                while i < $n {
                    out[i] = self.bytes[i] & rhs.bytes[i];
                    i += 1;
                }
                Self { bytes: out }
            }
            pub const fn const_shl(self, rhs: usize) -> Self {
                if $n == 0 { return self; }

                let byte_shift = rhs / 8;
                let bit_shift  = rhs % 8;

                if byte_shift >= $n {
                    return Self { bytes: [0u8; $n] };
                }

                let mut out = [0u8; $n];
                let mut i = 0;
                while i < $n {
                    let j = i + byte_shift;
                    if j >= $n { break; }

                    let mut b = self.bytes[j] << bit_shift;
                    if bit_shift != 0 && j + 1 < $n {
                        b |= self.bytes[j + 1] >> (8 - bit_shift);
                    }
                    out[i] = b;
                    i += 1;
                }

                Self { bytes: out }
            }
            pub const fn const_shr(self, rhs: usize) -> Self {
                if $n == 0 { return self; }

                let byte_shift = rhs / 8;
                let bit_shift  = rhs % 8;

                if byte_shift >= $n {
                    return Self { bytes: [0u8; $n] };
                }

                let mut out = [0u8; $n];
                let mut k = $n;
                while k > 0 {
                    let i = k - 1;
                    if i < byte_shift { break; }
                    let j = i - byte_shift;

                    let mut b = self.bytes[j] >> bit_shift;
                    if bit_shift != 0 && j > 0 {
                        b |= self.bytes[j - 1] << (8 - bit_shift);
                    }
                    out[i] = b;
                    k -= 1;
                }

                Self { bytes: out }
            }
        }
        impl Shl<usize> for SizedBytesImpl<$n> {
            type Output = Self;

            fn shl(self, rhs: usize) -> Self::Output {
                self.const_shl(rhs)
            }
        }
        impl ShlAssign<usize> for SizedBytesImpl<$n> {
            fn shl_assign(&mut self, rhs: usize) {
                *self = *self << rhs;
            }
        }
        impl Shr<usize> for SizedBytesImpl<$n> {
            type Output = Self;

            fn shr(self, rhs: usize) -> Self::Output {
                self.const_shr(rhs)
            }
        }

        impl ShrAssign<usize> for SizedBytesImpl<$n> {
            fn shr_assign(&mut self, rhs: usize) {
                *self = *self >> rhs;
            }
        }
    )+};
}
impl_const_sized_bytes!(4, 8, 32, 48, 96, 100, 480);
impl<const SIZE: usize> TryFrom<SExpNumber> for SizedBytesImpl<SIZE> {
    type Error = Error;
    fn try_from(value: SExpNumber) -> Result<Self, Error> {
        let bytes = match value {
            SExpNumber::I128(value) => value.to_be_bytes().to_vec(),
            SExpNumber::BigInt(value) => value.to_bytes_be().1,
        };
        if bytes.len() > SIZE {
            Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "i128 Too Large, expected {} bytes got {}",
                    SIZE,
                    bytes.len()
                ),
            ))
        } else {
            let mut buf = [0u8; SIZE];
            let offset = SIZE - bytes.len();
            for (i, v) in bytes.iter().enumerate() {
                buf[offset + i] = *v;
            }
            Ok(Self { bytes: buf })
        }
    }
}
impl<const SIZE: usize> SizedBytes<'_, SIZE> for SizedBytesImpl<SIZE> {
    const SIZE: usize = SIZE;
    fn new(bytes: [u8; SIZE]) -> Self {
        Self { bytes }
    }
    fn parse(bytes: &[u8]) -> Result<Self, ClvmError> {
        let mut buf = [0u8; SIZE];
        if bytes.len() > SIZE {
            Err(ClvmError::InvalidInput(format!(
                "Too Many Bytes Sent to parse, expected {} got {}",
                SIZE,
                bytes.len()
            )))
        } else {
            let offset = SIZE - bytes.len();
            for (i, v) in bytes.iter().enumerate() {
                buf[offset + i] = *v;
            }
            Ok(Self { bytes: buf })
        }
    }

    fn bytes(&self) -> [u8; SIZE] {
        self.bytes
    }
}

impl<const SIZE: usize> BitXor for SizedBytesImpl<SIZE> {
    type Output = Self;

    fn bitxor(self, rhs: Self) -> Self::Output {
        self ^ rhs.bytes
    }
}
impl<const SIZE: usize> BitXor<[u8; SIZE]> for SizedBytesImpl<SIZE> {
    type Output = Self;

    fn bitxor(self, rhs: [u8; SIZE]) -> Self::Output {
        let mut output: [u8; SIZE] = [0; SIZE];
        for ((x, y), o) in self.bytes.iter().zip(rhs.iter()).zip(output.iter_mut()) {
            *o = x ^ y;
        }
        Self::new(output)
    }
}
impl<const SIZE: usize> BitAnd for SizedBytesImpl<SIZE> {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        self & rhs.bytes
    }
}
impl<const SIZE: usize> BitAnd<[u8; SIZE]> for SizedBytesImpl<SIZE> {
    type Output = Self;

    fn bitand(self, rhs: [u8; SIZE]) -> Self::Output {
        let mut output: [u8; SIZE] = [0; SIZE];
        for ((x, y), o) in self.bytes.iter().zip(rhs.iter()).zip(output.iter_mut()) {
            *o = x & y;
        }
        Self::new(output)
    }
}
impl<const SIZE: usize> BitOr for SizedBytesImpl<SIZE> {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self | rhs.bytes
    }
}
impl<const SIZE: usize> BitOr<[u8; SIZE]> for SizedBytesImpl<SIZE> {
    type Output = Self;

    fn bitor(self, rhs: [u8; SIZE]) -> Self::Output {
        let mut output: [u8; SIZE] = [0; SIZE];
        for ((x, y), o) in self.bytes.iter().zip(rhs.iter()).zip(output.iter_mut()) {
            *o = x | y;
        }
        Self::new(output)
    }
}
impl<const SIZE: usize> Fill for SizedBytesImpl<SIZE> {
    fn fill_slice<R: Rng + ?Sized>(this: &mut [Self], rng: &mut R) {
        for slice in this.iter_mut() {
            let mut_slice: &mut [u8] = slice.deref_mut();
            rng.fill_bytes(mut_slice);
        }
    }
}
impl<const SIZE: usize> FromStr for SizedBytesImpl<SIZE> {
    type Err = ClvmError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.try_into()
    }
}
impl<const SIZE: usize> std::hash::Hash for SizedBytesImpl<SIZE> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}
impl<const SIZE: usize> PartialEq for SizedBytesImpl<SIZE> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}
impl<const SIZE: usize> Eq for SizedBytesImpl<SIZE> {}
impl<const SIZE: usize> Serialize for SizedBytesImpl<SIZE> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.to_string().as_str())
    }
}
impl<const SIZE: usize> Index<usize> for SizedBytesImpl<SIZE> {
    type Output = u8;
    fn index(&self, index: usize) -> &Self::Output {
        &self.bytes[index]
    }
}
impl<N: AsPrimitive<usize>, const SIZE: usize> Index<Range<N>> for SizedBytesImpl<SIZE> {
    type Output = [u8];
    fn index(&self, index: Range<N>) -> &Self::Output {
        &self.bytes[index.start.as_()..index.end.as_()]
    }
}
impl<N: AsPrimitive<usize>, const SIZE: usize> IndexMut<Range<N>> for SizedBytesImpl<SIZE> {
    fn index_mut(&mut self, index: Range<N>) -> &mut Self::Output {
        &mut self.bytes[index.start.as_()..index.end.as_()]
    }
}
impl<const SIZE: usize> AsRef<[u8]> for SizedBytesImpl<SIZE> {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl<const SIZE: usize> AsRef<[u8; SIZE]> for SizedBytesImpl<SIZE> {
    fn as_ref(&self) -> &[u8; SIZE] {
        &self.bytes
    }
}
impl<const SIZE: usize> From<SizedBytesImpl<SIZE>> for Vec<u8> {
    fn from(slf: SizedBytesImpl<SIZE>) -> Vec<u8> {
        slf.bytes.to_vec()
    }
}
impl<const SIZE: usize> From<[u8; SIZE]> for SizedBytesImpl<SIZE> {
    fn from(bytes: [u8; SIZE]) -> SizedBytesImpl<SIZE> {
        SizedBytesImpl { bytes }
    }
}
impl<const SIZE: usize> From<Vec<u8>> for SizedBytesImpl<SIZE> {
    fn from(vec: Vec<u8>) -> SizedBytesImpl<SIZE> {
        if vec.len() > SIZE {
            warn!(
                "Vec of size {} was truncated to fit a SizedBytes of size {}",
                vec.len(),
                SIZE
            );
        }
        let mut bytes = [0; SIZE];
        bytes[0..min(SIZE, vec.len())].copy_from_slice(&vec[0..min(SIZE, vec.len())]);
        SizedBytesImpl { bytes }
    }
}
impl<const SIZE: usize> IntoIterator for SizedBytesImpl<SIZE> {
    type Item = u8;
    type IntoIter = core::array::IntoIter<u8, { SIZE }>;

    fn into_iter(self) -> Self::IntoIter {
        self.bytes.into_iter()
    }
}
impl<const SIZE: usize> TryFrom<&str> for SizedBytesImpl<SIZE> {
    type Error = ClvmError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(&hex::decode(prep_hex_str(value)).map_err(|e| {
            ClvmError::InvalidInput(format!(
                "Hex string {value} is not a Valid Bytes{SIZE}: {e:?}"
            ))
        })?)
    }
}
impl<const SIZE: usize> std::fmt::Display for SizedBytesImpl<SIZE> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{}", encode(self.bytes))
    }
}
impl<const SIZE: usize> Default for SizedBytesImpl<SIZE> {
    fn default() -> SizedBytesImpl<SIZE> {
        SizedBytesImpl::new([0; SIZE])
    }
}
impl<const SIZE: usize> std::fmt::Debug for SizedBytesImpl<SIZE> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{}", encode(self.bytes))
    }
}
#[cfg(feature = "postgres")]
impl<'r, const SIZE: usize> sqlx::Decode<'r, sqlx::Postgres> for SizedBytesImpl<SIZE> {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let bytes = <&[u8] as sqlx::Decode<'_, sqlx::Postgres>>::decode(value)?;
        Ok(Self::parse(bytes)
            .map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e.to_string()))?)
    }
}
#[cfg(feature = "postgres")]
impl<'r, const SIZE: usize> sqlx::Encode<'r, sqlx::Postgres> for SizedBytesImpl<SIZE> {
    fn encode(
        self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer<'r>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError>
    where
        Self: Sized,
    {
        buf.extend_from_slice(&self);
        Ok(sqlx::encode::IsNull::No)
    }
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer<'r>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        buf.extend_from_slice(self);
        Ok(sqlx::encode::IsNull::No)
    }
    fn size_hint(&self) -> usize {
        SIZE
    }
}
#[cfg(feature = "sqlite")]
impl<'r, const SIZE: usize> sqlx::Decode<'r, sqlx::Sqlite> for SizedBytesImpl<SIZE> {
    fn decode(value: sqlx::sqlite::SqliteValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let bytes = <&[u8] as sqlx::Decode<'_, sqlx::Sqlite>>::decode(value)?;
        Ok(Self::parse(bytes)
            .map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e.to_string()))?)
    }
}
#[cfg(feature = "sqlite")]
impl<'q, const SIZE: usize> sqlx::Encode<'q, sqlx::Sqlite> for SizedBytesImpl<SIZE> {
    fn encode(
        self,
        buf: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError>
    where
        Self: Sized,
    {
        buf.push(sqlx::sqlite::SqliteArgumentValue::Blob(
            std::borrow::Cow::Owned(self.bytes.to_vec()),
        ));
        Ok(sqlx::encode::IsNull::No)
    }
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        buf.push(sqlx::sqlite::SqliteArgumentValue::Blob(
            std::borrow::Cow::Owned(self.bytes.to_vec()),
        ));
        Ok(sqlx::encode::IsNull::No)
    }
    fn size_hint(&self) -> usize {
        SIZE
    }
}
struct SizedBytesImplVisitor<const SIZE: usize>;
impl<const SIZE: usize> Visitor<'_> for SizedBytesImplVisitor<SIZE> {
    type Value = SizedBytesImpl<SIZE>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter
            .write_str(format!("Expecting a hex String, or byte array of size {SIZE}").as_str())
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Self::Value::try_from(value).map_err(|e| serde::de::Error::custom(e.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Self::Value::try_from(value.as_str()).map_err(|e| serde::de::Error::custom(e.to_string()))
    }
}
impl<'a, const SIZE: usize> Deserialize<'a> for SizedBytesImpl<SIZE> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        match deserializer.deserialize_string(SizedBytesImplVisitor) {
            Ok(hex) => Ok(hex),
            Err(er) => Err(er),
        }
    }
}

macro_rules! impl_add {
    ($($n:expr, $m:expr),+ $(,)?) => {
        $(
            impl Add for SizedBytesImpl<$n> {
                type Output = SizedBytesImpl<$m>;

                fn add(self, rhs: Self) -> Self::Output {
                    let mut out = [0u8; $m];
                    out[..$n].copy_from_slice(&self.bytes);
                    out[$n..].copy_from_slice(&rhs.bytes);
                    SizedBytesImpl { bytes: out }
                }
            }
        )+
    };
}

impl_add!(16, 32, 32, 64, 48, 96, 64, 128, 128, 256, 256, 512,);

macro_rules! impl_split {
    ($($n:expr, $m:expr);+ $(;)?) => {
        $(
            impl SizedBytesImpl<$m> {
                pub fn split(self) -> ([u8; $n], [u8; $n]) {
                    #[repr(C)]
                    #[derive(Clone, Copy)]
                    struct Halves {
                        pub a: [u8; $n],
                        pub b: [u8; $n],
                    }
                    let halves = unsafe { std::mem::transmute::<[u8; $m], Halves>(self.bytes) };
                    (halves.a, halves.b)
                }
            }
        )+
    };
}

impl_split!(16, 32; 32, 64; 48, 96; 64, 128; 128, 256; 256, 512;);

macro_rules! impl_sized_bytes {
    ($($name: ident, $size:expr);*) => {
        $(
            pub type $name = SizedBytesImpl<$size>;
            impl DefaultIsZeroes for SizedBytesImpl<$size> {}
            impl<'a> TryFrom<Program<'a>> for $name {
                type Error = ClvmError;
                fn try_from(value: Program) -> Result<Self, Self::Error> {
                    let vec = value.as_vec().ok_or_else(|| ClvmError::InvalidInput(format!("Program is not a valid {}",  stringify!($name))))?;
                    Self::parse(&vec)
                }
            }
            impl<'a> TryFrom<&Program<'a>> for $name {
                type Error = ClvmError;
                fn try_from(value: &Program) -> Result<Self, Self::Error> {
                    let vec = value.as_vec().ok_or_else(|| ClvmError::InvalidInput(format!("Program is not a valid {}",  stringify!($name))))?;
                    Self::parse(&vec)
                }
            }
            impl TryFrom<SExp<'_>> for $name {
                type Error = ClvmError;
                fn try_from(value: SExp) -> Result<Self, Self::Error> {
                    let vec = value.as_vec().ok_or_else(|| ClvmError::InvalidInput(format!("SExp is not a valid {}",  stringify!($name))))?;
                    Self::parse(&vec)
                }
            }
            impl TryFrom<&SExp<'_>> for $name {
                type Error = ClvmError;
                fn try_from(value: &SExp) -> Result<Self, Self::Error> {
                    let vec = value.as_vec().ok_or_else(|| ClvmError::InvalidInput(format!("SExp is not a valid {}",  stringify!($name))))?;
                    Self::parse(&vec)
                }
            }
            #[cfg(feature = "postgres")]
            impl sqlx::Type<sqlx::Postgres> for $name {
                fn type_info() -> sqlx::postgres::PgTypeInfo {
                    <Vec<u8> as sqlx::Type<sqlx::Postgres>>::type_info()
                }
                fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
                    <Vec<u8> as sqlx::Type<sqlx::Postgres>>::compatible(ty)
                }
            }
            #[cfg(feature = "sqlite")]
            impl sqlx::Type<sqlx::Sqlite> for $name {
                fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
                    <Vec<u8> as sqlx::Type<sqlx::Sqlite>>::type_info()
                }
                fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
                    <Vec<u8> as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
                }
            }
            impl ChiaSerialize for $name {
                fn append_bytes(&self, bytes: &mut Vec<u8>, _version: ChiaProtocolVersion) -> Result<(), Error> {
                    bytes.extend_from_slice(&self.bytes);
                    Ok(())
                }
                fn to_bytes(&self, _version: ChiaProtocolVersion) -> Result<Vec<u8>, std::io::Error> {
                    Ok(self.bytes().to_vec())
                }
                fn from_bytes(bytes: &mut Cursor<&[u8]>, _version: ChiaProtocolVersion) -> Result<Self, Error> where Self: Sized,
                {
                    if bytes.remaining() < $size {
                        Err(Error::new(ErrorKind::InvalidInput, format!("Failed to Parse {}, expected length {}, found {}", stringify!($name),  $size, bytes.remaining())))
                    } else {
                        let mut buf = [0u8; $size];
                        bytes.read_exact(&mut buf)?;
                        Ok(buf.into())
                    }
                }
            }
        )*
    };
    ()=>{};
}

impl_sized_bytes!(
    Bytes4, 4;
    Bytes8, 8;
    Bytes32, 32;
    Bytes48, 48;
    Bytes64, 64;
    Bytes96, 96;
    Bytes100, 100;
    Bytes480, 480
);

// Sized-bytes <-> BLS key/signature conversions live behind the `bls` feature: they are the
// only part of this module that touches `blst`, and the plain byte types must stay usable in
// builds that carry no BLS cryptography.
#[cfg(feature = "bls")]
mod bls_conversions {
    use super::{Bytes32, Bytes48, Bytes96};
    use crate::traits::SizedBytes;
    use blst::min_pk::{PublicKey, SecretKey, Signature};
    use std::io::{Error, ErrorKind};

    impl From<&Bytes32> for SecretKey {
        fn from(val: &Bytes32) -> Self {
            SecretKey::from_bytes(val.as_ref()).unwrap_or_default()
        }
    }
    impl From<Bytes32> for SecretKey {
        fn from(val: Bytes32) -> Self {
            SecretKey::from_bytes(val.as_ref()).unwrap_or_default()
        }
    }

    impl From<&SecretKey> for Bytes32 {
        fn from(val: &SecretKey) -> Self {
            Bytes32::new(val.to_bytes())
        }
    }
    impl From<SecretKey> for Bytes32 {
        fn from(val: SecretKey) -> Self {
            Bytes32::new(val.to_bytes())
        }
    }

    impl From<&Bytes48> for PublicKey {
        fn from(val: &Bytes48) -> Self {
            PublicKey::from_bytes(val.as_ref()).unwrap_or_default()
        }
    }
    impl From<Bytes48> for PublicKey {
        fn from(val: Bytes48) -> Self {
            PublicKey::from_bytes(val.as_ref()).unwrap_or_default()
        }
    }
    impl From<&PublicKey> for Bytes48 {
        fn from(val: &PublicKey) -> Self {
            Bytes48::new(val.to_bytes())
        }
    }
    impl From<PublicKey> for Bytes48 {
        fn from(val: PublicKey) -> Self {
            (&val).into()
        }
    }
    impl TryFrom<&Bytes96> for Signature {
        type Error = Error;

        fn try_from(val: &Bytes96) -> Result<Signature, Error> {
            Signature::from_bytes(val.as_ref())
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))
        }
    }

    impl TryFrom<Bytes96> for Signature {
        type Error = Error;

        fn try_from(val: Bytes96) -> Result<Signature, Error> {
            Signature::from_bytes(val.as_ref())
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))
        }
    }
    impl From<&Signature> for Bytes96 {
        fn from(val: &Signature) -> Bytes96 {
            Bytes96::new(val.to_bytes())
        }
    }

    impl From<Signature> for Bytes96 {
        fn from(val: Signature) -> Bytes96 {
            Bytes96::new(val.to_bytes())
        }
    }
}

#[cfg(all(test, feature = "postgres", feature = "sqlite"))]
mod codec_tests {
    use super::Bytes32;
    use crate::traits::SizedBytes;

    #[test]
    fn bytes32_round_trips_through_pg_and_sqlite_codecs() {
        let original = Bytes32::from(std::array::from_fn::<u8, 32, _>(|i| i as u8));

        let mut pg_buf = sqlx::postgres::PgArgumentBuffer::default();
        let _ = <Bytes32 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&original, &mut pg_buf)
            .unwrap();
        let pg_bytes: &[u8] = &pg_buf;
        assert_eq!(Bytes32::parse(pg_bytes).unwrap(), original);

        let mut sq_buf: Vec<sqlx::sqlite::SqliteArgumentValue> = Vec::new();
        let _ =
            <Bytes32 as sqlx::Encode<sqlx::Sqlite>>::encode_by_ref(&original, &mut sq_buf).unwrap();
        let sqlx::sqlite::SqliteArgumentValue::Blob(sq_bytes) = &sq_buf[0] else {
            panic!("sqlite encode did not produce a Blob");
        };
        assert_eq!(Bytes32::parse(sq_bytes).unwrap(), original);

        assert_eq!(pg_bytes, sq_bytes.as_ref());
    }
}
