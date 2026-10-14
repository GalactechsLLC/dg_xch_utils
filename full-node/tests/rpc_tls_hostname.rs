use dg_full_node::{RpcTlsMode, build_portfu_rpc_tls_context_for_host};
use dg_xch_core::ssl::{load_certs_from_bytes, make_ca_cert};
use rustls::RootCertStore;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{ServerName, UnixTime};
use std::sync::Arc;

#[test]
fn node_certificate_verifies_its_kubernetes_dns_name_and_rejects_other_nodes() {
    let directory = tempfile::tempdir().unwrap();
    let ca = directory.path().join("ca");
    std::fs::create_dir(&ca).unwrap();
    let (certificate, _) =
        make_ca_cert(&ca.join("private_ca.crt"), &ca.join("private_ca.key")).unwrap();
    let host = "dgx-nodes-0.dgx-nodes-headless.dg-pos2-test.svc.cluster.local";
    let context = build_portfu_rpc_tls_context_for_host(
        &RpcTlsMode::PrivateCa {
            ssl_dir: directory.path().into(),
        },
        host,
    )
    .unwrap();
    assert_eq!(context.tls_config.identities[0].domain, host);
    let certificates =
        load_certs_from_bytes(&context.tls_config.identities[0].cert_chain_pem).unwrap();
    let mut roots = RootCertStore::empty();
    for certificate in load_certs_from_bytes(&certificate).unwrap() {
        roots.add(certificate).unwrap();
    }
    let verifier = WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()
    .unwrap();
    assert!(
        verifier
            .verify_server_cert(
                &certificates[0],
                &certificates[1..],
                &ServerName::try_from(host).unwrap(),
                &[],
                UnixTime::now()
            )
            .is_ok()
    );
    assert!(
        verifier
            .verify_server_cert(
                &certificates[0],
                &certificates[1..],
                &ServerName::try_from(
                    "dgx-nodes-1.dgx-nodes-headless.dg-pos2-test.svc.cluster.local"
                )
                .unwrap(),
                &[],
                UnixTime::now()
            )
            .is_err()
    );
    assert!(
        verifier
            .verify_server_cert(
                &certificates[0],
                &certificates[1..],
                &ServerName::try_from("localhost").unwrap(),
                &[],
                UnixTime::now()
            )
            .is_err()
    );
}

#[test]
fn invalid_tls_hostname_is_rejected_before_ca_creation() {
    let directory = tempfile::tempdir().unwrap();
    assert!(
        build_portfu_rpc_tls_context_for_host(
            &RpcTlsMode::PrivateCa {
                ssl_dir: directory.path().into()
            },
            "https://not-a-host/"
        )
        .is_err()
    );
    assert!(!directory.path().join("ca").exists());
}
