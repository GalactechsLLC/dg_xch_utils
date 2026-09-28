use dg_xch_macros::ChiaSerial;
use rustls::DigitallySignedStruct;
use rustls::SignatureScheme;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use serde::{Deserialize, Serialize};

pub enum Capability {
    Base = 1,
    BlockHeaders = 2,
    RateLimitsV2 = 3,
    NoneResponse = 4,
    // The mempool-update push surface — deliberately NOT advertised.
    MempoolUpdates = 5,
    // Signals Hard Fork 2 support — pre-activation on mainnet (HF2 standing bucket).
    HardFork2 = 6,
    RateLimitsV3 = 7,
}

pub type Capabilities = Vec<(u16, String)>;

#[derive(ChiaSerial, Serialize, Deserialize, Debug, Clone)]
pub struct Handshake {
    //Same for all Versions
    pub network_id: String,         //Min Version 0.0.34
    pub protocol_version: String,   //Min Version 0.0.34
    pub software_version: String,   //Min Version 0.0.34
    pub server_port: u16,           //Min Version 0.0.34
    pub node_type: u8,              //Min Version 0.0.34
    pub capabilities: Capabilities, //Min Version 0.0.34
}

/// The `error` protocol message body (message-type code 255). Peers at protocol 0.0.35 and
/// above send it in place of a typed reject when a handler errors, and tolerate receiving it: an
/// inbound `error` is decoded and logged, then the link carries on — no ban, no disconnect. Named
/// `ErrorMessage` rather than `Error` to keep `std::io::Error` unambiguous at use sites. `data`
/// streams as a u32 length prefix plus raw bytes — `Vec<u8>`'s exact wire shape.
#[derive(ChiaSerial, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ErrorMessage {
    pub code: i16,
    pub message: String,
    pub data: Option<Vec<u8>>,
}

/// The rate-limits-v3 handshake follow-up (message-type code 111): the sender's window sizes per
/// message type — `(message_type_code, window_size)` with 0 meaning unlimited. See
/// `crate::protocols::rate_limits_v3`.
#[derive(ChiaSerial, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ConfigureWindowSizes {
    pub settings: Vec<(u8, u16)>,
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
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &crate::ssl::peer_signature_algorithms(),
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &crate::ssl::peer_signature_algorithms(),
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        crate::ssl::peer_signature_algorithms().supported_schemes()
    }
}
