use dg_xch_core::{
    constants::CHIA_CA_CRT,
    protocols::shared::{Capability, Handshake, NoCertificateVerification},
    ssl::{load_certs_from_bytes, peer_signature_algorithms},
};
use dg_xch_serialize::ChiaProtocolVersion;
use rustls::{
    DigitallySignedStruct, RootCertStore, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use std::io::Error;

// Public CA renewed by CNI in November 2025; keep the legacy anchor for older peers.
pub const CURRENT_CA: &str = include_str!("chia_ca.crt");

// CNI advertises this for FULL_NODE and INTRODUCER, independently of other roles.
pub const VERSION: ChiaProtocolVersion = ChiaProtocolVersion::Chia0_0_36;
pub const CAPABILITIES: [(u16, &str); 3] = [
    (Capability::Base as u16, "1"),
    (Capability::RateLimitsV2 as u16, "1"),
    // The observation-only wire subset remains compatible across HF2. CNI full
    // nodes reject incoming full-node peers lacking this signal after activation.
    (Capability::HardFork2 as u16, "1"),
];

pub fn codec(hello: &Handshake) -> Result<ChiaProtocolVersion, Error> {
    let parts: Vec<_> = hello.protocol_version.split('.').collect();
    let patch = if parts.len() == 3 && parts[0] == "0" && parts[1] == "0" {
        parts[2].parse::<u32>().ok()
    } else {
        None
    };
    // The crawler's wire subset is identical in CNI 0.0.32 through current releases.
    // Keep a newer peer's announced version intact; use our supported wire subset.
    let version = match patch {
        Some(32..=34) => ChiaProtocolVersion::Chia0_0_34,
        Some(35) => ChiaProtocolVersion::Chia0_0_35,
        Some(36..) => VERSION,
        _ => {
            return Err(Error::other(format!(
                "unsupported peer protocol version: {}",
                hello.protocol_version
            )));
        }
    };
    if hello.software_version.len() > 128 {
        return Err(Error::other("handshake software version exceeds 128 bytes"));
    }
    if !hello
        .capabilities
        .iter()
        .any(|(id, state)| *id == Capability::Base as u16 && state == "1")
    {
        return Err(Error::other(
            "handshake does not enable the base Chia protocol",
        ));
    }
    Ok(version)
}

/// Chia peers authenticate to the shared Chia CA, without DNS/IP name checks.
#[derive(Debug)]
pub struct ChiaCertificateVerifier {
    roots: RootCertStore,
}
impl ChiaCertificateVerifier {
    pub fn new() -> Result<Self, Error> {
        let mut roots = RootCertStore::empty();
        for pem in [CURRENT_CA, CHIA_CA_CRT] {
            for cert in load_certs_from_bytes(pem.as_bytes())? {
                roots.add(cert).map_err(Error::other)?;
            }
        }
        Ok(Self { roots })
    }
}
impl ServerCertVerifier for ChiaCertificateVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let cert = rustls::server::ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_cert_signed_by_trust_anchor(
            &cert,
            &self.roots,
            intermediates,
            now,
            peer_signature_algorithms().all,
        )?;
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        NoCertificateVerification.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        NoCertificateVerification.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        peer_signature_algorithms().supported_schemes()
    }
}
