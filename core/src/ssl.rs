use log::{error, info};
use openssl::asn1::Asn1Time;
use openssl::bn::{BigNum, MsbOption};
use openssl::error::ErrorStack;
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::PKey;
use openssl::rsa::Rsa;
use openssl::x509::extension::{BasicConstraints, SubjectAlternativeName};
use openssl::x509::{X509Builder, X509NameBuilder, X509};
use std::collections::HashMap;
use std::fs;
use std::fs::{create_dir_all, OpenOptions};
use std::io::{Error, ErrorKind, Write};
use std::ops::{Add, Sub};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const CHIA_CA_CRT: &str = r"-----BEGIN CERTIFICATE-----
MIIDKTCCAhGgAwIBAgIUXIpxI5MoZQ65/vhc7DK/d5ymoMUwDQYJKoZIhvcNAQEL
BQAwRDENMAsGA1UECgwEQ2hpYTEQMA4GA1UEAwwHQ2hpYSBDQTEhMB8GA1UECwwY
T3JnYW5pYyBGYXJtaW5nIERpdmlzaW9uMB4XDTIxMDEyMzA4NTEwNloXDTMxMDEy
MTA4NTEwNlowRDENMAsGA1UECgwEQ2hpYTEQMA4GA1UEAwwHQ2hpYSBDQTEhMB8G
A1UECwwYT3JnYW5pYyBGYXJtaW5nIERpdmlzaW9uMIIBIjANBgkqhkiG9w0BAQEF
AAOCAQ8AMIIBCgKCAQEAzz/L219Zjb5CIKnUkpd2julGC+j3E97KUiuOalCH9wdq
gpJi9nBqLccwPCSFXFew6CNBIBM+CW2jT3UVwgzjdXJ7pgtu8gWj0NQ6NqSLiXV2
WbpZovfrVh3x7Z4bjPgI3ouWjyehUfmK1GPIld4BfUSQtPlUJ53+XT32GRizUy+b
0CcJ84jp1XvyZAMajYnclFRNNJSw9WXtTlMUu+Z1M4K7c4ZPwEqgEnCgRc0TCaXj
180vo7mCHJQoDiNSCRATwfH+kWxOOK/nePkq2t4mPSFaX8xAS4yILISIOWYn7sNg
dy9D6gGNFo2SZ0FR3x9hjUjYEV3cPqg3BmNE3DDynQIDAQABoxMwETAPBgNVHRMB
Af8EBTADAQH/MA0GCSqGSIb3DQEBCwUAA4IBAQAEugnFQjzHhS0eeCqUwOHmP3ww
/rXPkKF+bJ6uiQgXZl+B5W3m3zaKimJeyatmuN+5ST1gUET+boMhbA/7grXAsRsk
SFTHG0T9CWfPiuimVmGCzoxLGpWDMJcHZncpQZ72dcy3h7mjWS+U59uyRVHeiprE
hvSyoNSYmfvh7vplRKS1wYeA119LL5fRXvOQNW6pSsts17auu38HWQGagSIAd1UP
5zEvDS1HgvaU1E09hlHzlpdSdNkAx7si0DMzxKHUg9oXeRZedt6kcfyEmryd52Mj
1r1R9mf4iMIUv1zc2sHVc1omxnCw9+7U4GMWLtL5OgyJyfNyoxk3tC+D3KNU
-----END CERTIFICATE-----";

pub const CHIA_CA_KEY: &str = r"-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEAzz/L219Zjb5CIKnUkpd2julGC+j3E97KUiuOalCH9wdqgpJi
9nBqLccwPCSFXFew6CNBIBM+CW2jT3UVwgzjdXJ7pgtu8gWj0NQ6NqSLiXV2WbpZ
ovfrVh3x7Z4bjPgI3ouWjyehUfmK1GPIld4BfUSQtPlUJ53+XT32GRizUy+b0CcJ
84jp1XvyZAMajYnclFRNNJSw9WXtTlMUu+Z1M4K7c4ZPwEqgEnCgRc0TCaXj180v
o7mCHJQoDiNSCRATwfH+kWxOOK/nePkq2t4mPSFaX8xAS4yILISIOWYn7sNgdy9D
6gGNFo2SZ0FR3x9hjUjYEV3cPqg3BmNE3DDynQIDAQABAoIBAGupS4BJdx8gEAAh
2VDRqAAzhHTZb8j9uoKXJ+NotEkKrDTqUMiOu0nOqOsFWdYPo9HjxoggFuEU+Hpl
a4kj4uF3OG6Yj+jgLypjpV4PeoFM6M9R9BCp07In2i7DLLK9gvYA85SoVLBd/tW4
hFH+Qy3M+ZNZ1nLCK4pKjtaYs0dpi5zLoVvpEcEem2O+aRpUPCZqkNwU0umATCfg
ZGfFzgXI/XPJr8Uy+LVZOFp3PXXHfnZZD9T5AjO/ViBeqbMFuWQ8BpVOqapNPKj8
xDY3ovw3uiAYPC7eLib3u/WoFelMc2OMX0QljLp5Y+FScFHAMxoco3AQdWSYvSQw
b5xZmg0CgYEA6zKASfrw3EtPthkLR5NBmesI4RbbY6iFVhS5loLbzTtStvsus8EI
6RQgLgAFF14H21YSHxb6dB1Mbo45BN83gmDpUvKPREslqD3YPMKFo5GXMmv+JhNo
5Y9fhiOEnxzLJGtBB1HeGmg5NXp9mr2Ch9u8w/slfuCHckbA9AYvdxMCgYEA4ZR5
zg73+UA1a6Pm93bLYZGj+hf7OaB/6Hiw9YxCBgDfWM9dJ48iz382nojT5ui0rClV
5YAo8UCLh01Np9AbBZHuBdYm9IziuKNzTeK31UW+Tvbz+dEx7+PlYQffNOhcIgd+
9SXjoZorQksImKdMGZld1lEReHuBawq92JQvtY8CgYEAtNwUws7xQLW5CjKf9d5K
5+1Q2qYU9sG0JsmxHQhrtZoUtRjahOe/zlvnkvf48ksgh43cSYQF/Bw7lhhPyGtN
6DhVs69KdB3FS2ajTbXXxjxCpEdfHDB4zW4+6ouNhD1ECTFgxBw0SuIye+lBhSiN
o6NZuOr7nmFSRpIZ9ox7G3kCgYA4pvxMNtAqJekEpn4cChab42LGLX2nhFp7PMxc
bqQqM8/j0vg3Nihs6isCd6SYKjstvZfX8m7V3/rquQxWp9oRdQvNJXJVGojaDBqq
JdU7V6+qzzSIufQLpjV2P+7br7trxGwrDx/y9vAETynShLmE+FJrv6Jems3u3xy8
psKwmwKBgG5uLzCyMvMB2KwI+f3np2LYVGG0Pl1jq6yNXSaBosAiF0y+IgUjtWY5
EejO8oPWcb9AbqgPtrWaiJi17KiKv4Oyba5+y36IEtyjolWt0AB6F3oDK0X+Etw8
j/xlvBNuzDL6gRJHQg1+d4dO8Lz54NDUbKW8jGl+N/7afGVpGmX9
-----END RSA PRIVATE KEY-----";

pub fn generate_ca_signed_cert(
    cert_path: &Path,
    cert_data: &[u8],
    key_path: &Path,
    key_data: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let (cert_data, key_data) = generate_ca_signed_cert_data(cert_data, key_data)
        .map_err(|e| Error::new(ErrorKind::Other, format!("OpenSSL Errors: {:?}", e)))?;
    write_ssl_cert_and_key(cert_path, &cert_data, key_path, &key_data, true)?;
    Ok((cert_data, key_data))
}

fn write_ssl_cert_and_key(
    cert_path: &Path,
    cert_data: &[u8],
    key_path: &Path,
    key_data: &[u8],
    overwrite: bool,
) -> Result<(), Error> {
    if cert_path.exists() && overwrite {
        fs::remove_file(cert_path)?;
    }
    let mut crt = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(cert_path)?;
    crt.write_all(cert_data)?;
    crt.flush()?;
    if key_path.exists() && overwrite {
        fs::remove_file(key_path)?;
    }
    let mut key = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(key_path)?;
    key.write_all(key_data)?;
    key.flush()
}

pub fn generate_ca_signed_cert_data(
    cert_data: &[u8],
    key_data: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), ErrorStack> {
    let root_cert = X509::from_pem(cert_data)?;
    let root_key = PKey::from_rsa(Rsa::private_key_from_pem(key_data)?)?;
    let cert_key = Rsa::generate(2048)?;
    let pub_key = PKey::from_rsa(cert_key)?;
    let mut cert = X509Builder::new()?;
    let mut x509_name = X509NameBuilder::new()?;
    x509_name.append_entry_by_nid(Nid::COMMONNAME, "Chia")?;
    x509_name.append_entry_by_nid(Nid::ORGANIZATIONNAME, "Chia")?;
    x509_name.append_entry_by_nid(Nid::ORGANIZATIONALUNITNAME, "Organic Farming Division")?;
    let name = x509_name.build();
    cert.set_subject_name(name.as_ref())?;
    cert.set_issuer_name(root_cert.issuer_name())?;
    cert.set_pubkey(pub_key.as_ref())?;
    let mut bn = BigNum::new()?;
    bn.rand(32, MsbOption::MAYBE_ZERO, true)?;
    cert.set_serial_number(bn.to_asn1_integer()?.as_ref())?;
    cert.set_not_before(
        Asn1Time::from_unix(
            SystemTime::now()
                .sub(Duration::from_secs(60 * 60 * 24))
                .duration_since(UNIX_EPOCH)
                .expect("Should be later than Epoch")
                .as_secs() as i64,
        )?
        .as_ref(),
    )?;
    cert.set_not_after(Asn1Time::from_str("21000802000000Z")?.as_ref())?;
    let ctx = cert.x509v3_context(None, None);
    let san = SubjectAlternativeName::new().dns("chia.net").build(&ctx)?;
    cert.append_extension(san)?;
    cert.set_version(2)?;
    cert.sign(root_key.as_ref(), MessageDigest::sha256())?;
    let x509 = cert.build();
    Ok((x509.to_pem()?, pub_key.rsa()?.private_key_to_pem()?))
}

pub fn make_ca_cert(cert_path: &Path, key_path: &Path) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let (cert_data, key_data) = make_ca_cert_data()
        .map_err(|e| Error::new(ErrorKind::Other, format!("OpenSSL Errors: {:?}", e)))?;
    write_ssl_cert_and_key(cert_path, &cert_data, key_path, &key_data, true)?;
    Ok((cert_data, key_data))
}

fn make_ca_cert_data() -> Result<(Vec<u8>, Vec<u8>), ErrorStack> {
    let root_key = PKey::from_rsa(Rsa::generate(2048)?)?;
    let mut x509_name = X509NameBuilder::new()?;
    x509_name.append_entry_by_text("O", "Chia").unwrap();
    x509_name
        .append_entry_by_text("OU", "Organic Farming Division")
        .unwrap();
    x509_name.append_entry_by_text("CN", "Chia CA").unwrap();
    let mut cert = X509Builder::new()?;
    let name = x509_name.build();
    cert.set_subject_name(name.as_ref())?;
    cert.set_issuer_name(name.as_ref())?;
    cert.set_pubkey(root_key.as_ref())?;
    let mut bn = BigNum::new()?;
    bn.rand(32, MsbOption::MAYBE_ZERO, true)?;
    cert.set_serial_number(bn.to_asn1_integer()?.as_ref())?;
    cert.set_not_before(
        Asn1Time::from_unix(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Should be later than Epoch")
                .as_secs() as i64,
        )?
        .as_ref(),
    )?;
    cert.set_not_after(
        Asn1Time::from_unix(
            SystemTime::now()
                .add(Duration::from_secs(60 * 60 * 24 * 3650))
                .duration_since(UNIX_EPOCH)
                .expect("Should be later than Epoch")
                .as_secs() as i64,
        )?
        .as_ref(),
    )?;
    let base = BasicConstraints::new().critical().ca().build()?;
    cert.append_extension(base)?;
    cert.set_version(2)?;
    cert.sign(root_key.as_ref(), MessageDigest::sha256())?;
    let x509 = cert.build();
    Ok((x509.to_pem()?, root_key.rsa()?.private_key_to_pem()?))
}

const ALL_PRIVATE_NODE_NAMES: [&str; 8] = [
    "full_node",
    "wallet",
    "farmer",
    "harvester",
    "timelord",
    "crawler",
    "data_layer",
    "daemon",
];

const ALL_PUBLIC_NODE_NAMES: [&str; 6] = [
    "full_node",
    "wallet",
    "farmer",
    "introducer",
    "timelord",
    "data_layer",
];

pub struct MemorySSL {
    pub public: HashMap<String, MemoryNodeSSL>,
    pub private: HashMap<String, MemoryNodeSSL>,
}

pub struct MemoryNodeSSL {
    pub cert: Vec<u8>,
    pub key: Vec<u8>,
}

pub fn create_all_ssl_memory() -> Result<MemorySSL, Error> {
    info!("Generating CA Certs");
    let mut public_map = HashMap::new();
    let mut private_map = HashMap::new();
    let (ca_cert_data, ca_key_data) = make_ca_cert_data()
        .map_err(|e| Error::new(ErrorKind::Other, format!("OpenSSL Errors: {:?}", e)))?;
    info!("Generating Private Certs");
    let private_certs =
        generate_ssl_for_nodes_in_memory(&ca_cert_data, &ca_key_data, &ALL_PRIVATE_NODE_NAMES)?;
    private_map.insert(
        "ca".to_string(),
        MemoryNodeSSL {
            cert: ca_cert_data,
            key: ca_key_data,
        },
    );
    private_map.extend(private_certs.into_iter());
    info!("Generating Public Certs");
    let public_certs = generate_ssl_for_nodes_in_memory(
        CHIA_CA_CRT.as_bytes(),
        CHIA_CA_KEY.as_bytes(),
        &ALL_PUBLIC_NODE_NAMES,
    )?;
    public_map.insert(
        "ca".to_string(),
        MemoryNodeSSL {
            cert: CHIA_CA_CRT.as_bytes().to_vec(),
            key: CHIA_CA_KEY.as_bytes().to_vec(),
        },
    );
    public_map.extend(public_certs.into_iter());
    Ok(MemorySSL {
        public: public_map,
        private: private_map,
    })
}

pub fn create_all_ssl(ssl_dir: &Path, overwrite: bool) -> Result<(), Error> {
    let ca_dir = ssl_dir.join(Path::new("ca"));
    create_dir_all(&ca_dir)?;
    let private_ca_key_path = ca_dir.join("private_ca.key");
    let private_ca_crt_path = ca_dir.join("private_ca.crt");
    let chia_ca_crt_path = ca_dir.join("chia_ca.crt");
    let chia_ca_key_path = ca_dir.join("chia_ca.key");
    write_ssl_cert_and_key(
        &chia_ca_crt_path,
        CHIA_CA_CRT.as_bytes(),
        &chia_ca_key_path,
        CHIA_CA_KEY.as_bytes(),
        true,
    )?;
    let (crt, key) = if !private_ca_crt_path.exists() || !private_ca_key_path.exists() {
        info!("Generating SSL CA Cert");
        make_ca_cert(&private_ca_crt_path, &private_ca_key_path)?
    } else {
        info!("Loading SSL CA Cert");
        (
            fs::read(private_ca_crt_path)?,
            fs::read(private_ca_key_path)?,
        )
    };
    info!("Generating Private Certs");
    generate_ssl_for_nodes(
        ssl_dir,
        &crt,
        &key,
        "private",
        &ALL_PRIVATE_NODE_NAMES,
        overwrite,
    )?;
    info!("Generating Public Certs");
    generate_ssl_for_nodes(
        ssl_dir,
        CHIA_CA_CRT.as_bytes(),
        CHIA_CA_KEY.as_bytes(),
        "public",
        &ALL_PUBLIC_NODE_NAMES,
        overwrite,
    )
}

fn generate_ssl_for_nodes(
    ssl_dir: &Path,
    crt: &[u8],
    key: &[u8],
    prefix: &str,
    nodes: &[&str],
    overwrite: bool,
) -> Result<(), Error> {
    for node_name in nodes {
        let node_dir = ssl_dir.join(Path::new(*node_name));
        create_dir_all(&node_dir)?;
        let crt_path = node_dir.join(Path::new(&format!("{prefix}_{node_name}.crt")));
        let key_path = node_dir.join(Path::new(&format!("{prefix}_{node_name}.key")));
        if key_path.exists() && crt_path.exists() && !overwrite {
            continue;
        }
        if let Err(e) = generate_ca_signed_cert(&crt_path, crt, &key_path, key) {
            error!("Failed to write Cert Files: {:?}", e);
        }
    }
    Ok(())
}

pub fn generate_ssl_for_nodes_in_memory(
    crt: &[u8],
    key: &[u8],
    nodes: &[&str],
) -> Result<HashMap<String, MemoryNodeSSL>, Error> {
    let mut map = HashMap::new();
    for node_name in nodes {
        let (cert, key) = generate_ca_signed_cert_data(crt, key)
            .map_err(|e| Error::new(ErrorKind::Other, format!("OpenSSL Errors: {:?}", e)))?;
        map.insert(node_name.to_string(), MemoryNodeSSL { cert, key });
    }
    Ok(map)
}

#[test]
pub fn test_ssl() {
    use simple_logger::SimpleLogger;
    SimpleLogger::new().init().unwrap();
    create_all_ssl("./ssl".as_ref(), true).unwrap();
}
