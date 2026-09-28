use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::shared::NoCertificateVerification;
use dg_xch_core::ssl::{AllowAny, load_certs_from_bytes, load_private_key_from_bytes};
use rustls::client::danger::ServerCertVerifier;
use rustls::internal::msgs::codec::{Codec, Reader};
use rustls::server::danger::ClientCertVerifier;
use rustls::{DigitallySignedStruct, SignatureScheme};

#[test]
fn peer_identity_requires_possession_of_the_certificate_key() {
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    let secret = load_private_key_from_bytes(CHIA_CA_KEY.as_bytes()).unwrap();
    let key = provider.key_provider.load_private_key(secret).unwrap();
    let signer = key
        .choose_scheme(&[SignatureScheme::RSA_PSS_SHA256])
        .unwrap();
    let message = b"local dummy TLS transcript";
    let signature = signer.sign(message).unwrap();
    let mut encoded = Vec::new();
    signer.scheme().encode(&mut encoded);
    (signature.len() as u16).encode(&mut encoded);
    encoded.extend_from_slice(&signature);
    let signed = DigitallySignedStruct::read(&mut Reader::init(&encoded)).unwrap();
    let certificates = load_certs_from_bytes(CHIA_CA_CRT.as_bytes()).unwrap();
    let certificate = &certificates[0];
    let client_verifier = AllowAny::new();
    let server_verifier = NoCertificateVerification;
    assert!(client_verifier.client_auth_mandatory());
    assert!(
        client_verifier
            .verify_tls13_signature(message, certificate, &signed)
            .is_ok()
    );
    assert!(
        server_verifier
            .verify_tls13_signature(message, certificate, &signed)
            .is_ok()
    );
    assert!(
        client_verifier
            .verify_tls13_signature(b"forged", certificate, &signed)
            .is_err()
    );
    assert!(
        server_verifier
            .verify_tls13_signature(b"forged", certificate, &signed)
            .is_err()
    );
    assert!(
        client_verifier
            .verify_tls12_signature(b"forged", certificate, &signed)
            .is_err()
    );
    assert!(
        server_verifier
            .verify_tls12_signature(b"forged", certificate, &signed)
            .is_err()
    );
}
