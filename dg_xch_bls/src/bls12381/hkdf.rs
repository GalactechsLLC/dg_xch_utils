use hkdf::Hkdf;
use sha2::Sha256;
use std::io::{Error, ErrorKind};

pub fn extract_expand(
    length: usize,
    key: &[u8],
    salt: &[u8],
    info: &[u8],
) -> Result<Vec<u8>, Error> {
    let hk = Hkdf::<Sha256>::new(Some(salt), key);
    let mut out: Vec<u8> = (0..length).map(|_| 0).collect();
    match hk.expand(info, &mut out) {
        Ok(_) => Ok(out),
        Err(e) => Err(Error::new(ErrorKind::InvalidData, e.to_string())),
    }
}
