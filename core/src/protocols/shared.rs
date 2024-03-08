use dg_xch_macros::ChiaSerial;
use rustls::client::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{Certificate, DigitallySignedStruct, ServerName};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

pub enum Capability {
    Base = 1,
    BlockHeaders = 2,
    RateLimitsV2 = 3,
    NoneResponse = 4,
}

#[derive(ChiaSerial, Serialize, Deserialize, Debug, Clone)]
pub struct Handshake {
    //Same for all Versions
    pub network_id: String,               //Min Version 0.0.34
    pub protocol_version: String,         //Min Version 0.0.34
    pub software_version: String,         //Min Version 0.0.34
    pub server_port: u16,                 //Min Version 0.0.34
    pub node_type: u8,                    //Min Version 0.0.34
    pub capabilities: Vec<(u16, String)>, //Min Version 0.0.34
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
