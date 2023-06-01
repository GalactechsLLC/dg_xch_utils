use dg_xch_macros::ChiaSerial;
use log::error;
use rustls::client::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{Certificate, DigitallySignedStruct, PrivateKey, ServerName};
use rustls_pemfile::{certs, read_one, Item};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Error, ErrorKind};
use std::iter;
use std::time::SystemTime;

pub const PROTOCOL_VERSION: &str = "0.0.34";
pub const SOFTWARE_VERSION: &str = "dg_xch_utils_1_0_0";

pub enum Capability {
    Base = 1,
    BlockHeaders = 2,
    RateLimitsV2 = 3,
    NoneResponse = 4,
}

#[derive(ChiaSerial, Serialize, Deserialize, Debug, Clone)]
pub struct Handshake {
    pub network_id: String,
    pub protocol_version: String,
    pub software_version: String,
    pub server_port: u16,
    pub node_type: u8,
    pub capabilities: Vec<(u16, String)>,
}

pub const CAPABILITIES: [(u16, &str); 3] = [
    (Capability::Base as u16, "1"),
    (Capability::BlockHeaders as u16, "1"),
    (Capability::RateLimitsV2 as u16, "1"),
    //(Capability::NoneResponse as u16, "1"), //This is not currently supported, Causes the Fullnode to close the connection
];

pub struct NoCertificateVerification;

impl ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &Certificate,
        _intermediates: &[Certificate],
        _server_name: &ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: SystemTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &Certificate,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &Certificate,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
}

pub fn load_certs(filename: &str) -> Result<Vec<Certificate>, Error> {
    let cert_file = File::open(filename)?;
    let mut reader = BufReader::new(cert_file);
    let certs = certs(&mut reader)?;
    Ok(certs.into_iter().map(Certificate).collect())
}

pub fn load_private_key(filename: &str) -> Result<PrivateKey, Error> {
    let keyfile = File::open(filename)?;
    let mut reader = BufReader::new(keyfile);
    for item in iter::from_fn(|| read_one(&mut reader).transpose()) {
        match item? {
            Item::X509Certificate(_) => error!("Found Certificate, not Private Key"),
            Item::RSAKey(key) => {
                return Ok(PrivateKey(key));
            }
            Item::PKCS8Key(key) => {
                return Ok(PrivateKey(key));
            }
            Item::ECKey(key) => {
                return Ok(PrivateKey(key));
            }
            _ => error!("Unknown Item while loading private key"),
        }
    }
    Err(Error::new(ErrorKind::NotFound, "Private Key Not Found"))
}
