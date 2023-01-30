use crate::types::ChiaSerialize;
use log::error;
use rustls::client::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{DigitallySignedStruct, ServerName};
use rustls_pemfile::{certs, read_one, Item};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Error, ErrorKind};
use std::iter;
use std::time::SystemTime;
use tokio_rustls::rustls::{Certificate, PrivateKey};

pub const PROTOCOL_VERSION: &str = "0.0.34";
pub const SOFTWARE_VERSION: &str = "dg_xch_utils_1_0_0";

pub enum Capability {
    Base = 1,
    BlockHeaders = 2,
    RateLimitsV2 = 3,
    NoneResponse = 4,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Handshake {
    pub network_id: String,
    pub protocol_version: String,
    pub software_version: String,
    pub server_port: u16,
    pub node_type: u8,
    pub capabilities: Vec<(u16, String)>,
}
impl ChiaSerialize for Handshake {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend((self.network_id.len() as u32).to_be_bytes());
        bytes.extend(self.network_id.as_bytes());
        bytes.extend((self.protocol_version.len() as u32).to_be_bytes());
        bytes.extend(self.protocol_version.as_bytes());
        bytes.extend((self.software_version.len() as u32).to_be_bytes());
        bytes.extend(self.software_version.as_bytes());
        bytes.extend(self.server_port.to_be_bytes());
        bytes.push(self.node_type);
        bytes.extend((self.capabilities.len() as u32).to_be_bytes());
        for cap in &self.capabilities {
            bytes.extend(cap.0.to_be_bytes());
            bytes.extend((cap.1.len() as u32).to_be_bytes());
            bytes.extend(cap.1.as_bytes());
        }
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut str_len_ary: [u8; 4] = [0; 4];
        let mut u16_len_ary: [u8; 2] = [0; 2];
        let (network_len, rest) = bytes.split_at(4);
        str_len_ary.copy_from_slice(network_len);
        let (network_id, rest) = rest.split_at(u32::from_be_bytes(str_len_ary) as usize);
        let network_id = match String::from_utf8(network_id.to_vec()) {
            Ok(string) => string,
            Err(_) => String::new(),
        };

        let (protocol_len, rest) = rest.split_at(4);
        str_len_ary.copy_from_slice(&protocol_len[0..4]);
        let (protocol_version, rest) = rest.split_at(u32::from_be_bytes(str_len_ary) as usize);
        let protocol_version = match String::from_utf8(protocol_version.to_vec()) {
            Ok(string) => string,
            Err(_) => String::new(),
        };

        let (software_version_len, rest) = rest.split_at(4);
        str_len_ary.copy_from_slice(&software_version_len[0..4]);
        let (software_version, rest) = rest.split_at(u32::from_be_bytes(str_len_ary) as usize);
        let software_version = match String::from_utf8(software_version.to_vec()) {
            Ok(string) => string,
            Err(_) => String::new(),
        };

        let (server_port, rest) = rest.split_at(2);
        u16_len_ary.copy_from_slice(&server_port[0..2]);
        let server_port = u16::from_be_bytes(u16_len_ary);
        let (node_type, rest) = rest.split_at(1);
        let node_type = node_type[0];

        let (capabilities_len, rest) = rest.split_at(4);
        str_len_ary.copy_from_slice(&capabilities_len[0..4]);
        let capabilities_len = u32::from_be_bytes(str_len_ary) as usize;
        let mut capabilities = vec![];
        let mut rest = rest;
        for _ in 0..capabilities_len {
            let (cap, r) = rest.split_at(2);
            u16_len_ary.copy_from_slice(&cap[0..2]);
            let cap = u16::from_be_bytes(u16_len_ary);

            let (cap_str_len, r) = r.split_at(4);
            str_len_ary.copy_from_slice(&cap_str_len[0..4]);
            let (cap_str, r) = r.split_at(u32::from_be_bytes(str_len_ary) as usize);
            let cap_str = match String::from_utf8(cap_str.to_vec()) {
                Ok(string) => string,
                Err(_) => String::new(),
            };
            capabilities.push((cap, cap_str));
            rest = r;
        }
        Ok(Handshake {
            network_id,
            protocol_version,
            software_version,
            server_port,
            node_type,
            capabilities,
        })
    }
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
