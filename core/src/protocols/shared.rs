use dg_xch_macros::ChiaSerial;
use rustls::{DigitallySignedStruct};
use serde::{Deserialize, Serialize};
use rustls::SignatureScheme;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

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

#[derive(Debug)]
pub struct NoCertificateVerification;

impl ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::ECDSA_SHA1_Legacy,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}