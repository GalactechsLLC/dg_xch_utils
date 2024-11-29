use crate::constants::{ALL_PRIVATE_NODE_NAMES, ALL_PUBLIC_NODE_NAMES, CHIA_CA_CRT, CHIA_CA_KEY};
use der::asn1::{Ia5String, UtcTime};
use der::pem::LineEnding;
use der::{DateTime, EncodePem};
use log::{error, info};
use rand::Rng;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};
use rustls::server::{ClientCertVerified, ClientCertVerifier, ParsedCertificate};
use rustls::{DistinguishedName, PrivateKey};
use rustls_pemfile::{certs, read_one, Item};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::fs;
use std::fs::{create_dir_all, File, OpenOptions};
use std::io::{BufReader, Error, ErrorKind, Write};
use std::ops::{Add, Sub};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use x509_cert::builder::{Builder, CertificateBuilder, Profile};
use x509_cert::der::DecodePem;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::SubjectPublicKeyInfo;
use x509_cert::time::{Time, Validity};
use x509_cert::Certificate;

pub struct AllowAny {}
impl AllowAny {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {})
    }
}

impl ClientCertVerifier for AllowAny {
    fn client_auth_root_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn client_auth_mandatory(&self) -> bool {
        false
    }
    fn verify_client_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _now: SystemTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }
}

pub fn load_certs(filename: &str) -> Result<Vec<rustls::Certificate>, Error> {
    let cert_file = File::open(filename)?;
    let mut reader = BufReader::new(cert_file);
    let certs = certs(&mut reader)?;
    Ok(certs.into_iter().map(rustls::Certificate).collect())
}

pub fn load_certs_from_bytes(bytes: &[u8]) -> Result<Vec<rustls::Certificate>, Error> {
    let mut reader = BufReader::new(bytes);
    let certs = certs(&mut reader)?;
    Ok(certs.into_iter().map(rustls::Certificate).collect())
}

pub fn load_private_key(filename: &str) -> Result<PrivateKey, Error> {
    let keyfile = File::open(filename)?;
    let mut reader = BufReader::new(keyfile);
    for item in std::iter::from_fn(|| read_one(&mut reader).transpose()) {
        match item {
            Ok(Item::RSAKey(key) | Item::PKCS8Key(key) | Item::ECKey(key)) => {
                return Ok(PrivateKey(key));
            }
            Ok(Item::X509Certificate(_)) => error!("Found Certificate, not Private Key"),
            _ => {
                error!("Unknown Item while loading private key");
            }
        }
    }
    Err(Error::new(ErrorKind::NotFound, "Private Key Not Found"))
}

pub fn load_private_key_from_bytes(bytes: &[u8]) -> Result<PrivateKey, Error> {
    let mut reader = BufReader::new(bytes);
    for item in std::iter::from_fn(|| read_one(&mut reader).transpose()) {
        match item {
            Ok(Item::RSAKey(key) | Item::PKCS8Key(key) | Item::ECKey(key)) => {
                return Ok(PrivateKey(key));
            }
            Ok(Item::X509Certificate(_)) => error!("Found Certificate, not Private Key"),
            _ => {
                error!("Unknown Item while loading private key");
            }
        }
    }
    Err(Error::new(ErrorKind::NotFound, "Private Key Not Found"))
}

pub fn generate_ca_signed_cert(
    cert_path: &Path,
    cert_data: &[u8],
    key_path: &Path,
    key_data: &[u8],
    overwrite: bool,
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let (cert_data, key_data) = generate_ca_signed_cert_data(cert_data, key_data)?;
    write_ssl_cert_and_key(cert_path, &cert_data, key_path, &key_data, overwrite)?;
    Ok((cert_data, key_data))
}

fn write_ssl_cert_and_key(
    cert_path: &Path,
    cert_data: &[u8],
    key_path: &Path,
    key_data: &[u8],
    overwrite: bool,
) -> Result<(), Error> {
    let cert_exists = cert_path.exists();
    if !cert_exists || overwrite {
        if cert_exists {
            fs::remove_file(cert_path)?;
        }
        let mut crt = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(cert_path)?;
        crt.write_all(cert_data)?;
        crt.flush()?;
    }
    let key_exists = key_path.exists();
    if !key_exists || overwrite {
        if key_exists {
            fs::remove_file(key_path)?;
        }
        let mut key = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(key_path)?;
        key.write_all(key_data)?;
        key.flush()?;
    }
    Ok(())
}

pub fn generate_ca_signed_cert_data(
    cert_data: &[u8],
    key_data: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let root_cert = Certificate::from_pem(cert_data)
        .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
    let root_key = rsa::RsaPrivateKey::from_pkcs1_pem(&String::from_utf8_lossy(key_data))
        .or_else(|_| rsa::RsaPrivateKey::from_pkcs8_pem(&String::from_utf8_lossy(key_data)))
        .map_err(|e| Error::new(ErrorKind::Other, format!("Failed to load Root Key: {e:?}")))?;
    let mut rng = rand::thread_rng();
    let cert_key = rsa::RsaPrivateKey::new(&mut rng, 2048)
        .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
    let pub_key = cert_key.to_public_key();
    let signing_key: SigningKey<Sha256> = SigningKey::new(root_key);
    let subject_pub_key = SubjectPublicKeyInfo::from_pem(
        pub_key
            .to_public_key_pem(LineEnding::default())
            .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?
            .as_bytes(),
    )
    .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
    let mut cert = CertificateBuilder::new(
        Profile::Leaf {
            issuer: root_cert.tbs_certificate.issuer,
            enable_key_agreement: false,
            enable_key_encipherment: false,
        },
        SerialNumber::from(rng.gen::<u32>()),
        Validity {
            not_before: Time::UtcTime(
                UtcTime::from_system_time(SystemTime::now().sub(Duration::from_secs(60 * 60 * 24)))
                    .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?,
            ),
            not_after: Time::UtcTime(
                UtcTime::from_date_time(
                    DateTime::new(2049, 8, 2, 0, 0, 0)
                        .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?,
                )
                .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?,
            ),
        },
        Name::from_str("CN=Chia,O=Chia,OU=Organic Farming Division")
            .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?,
        subject_pub_key,
        &signing_key,
    )
    .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
    cert.add_extension(&SubjectAltName(vec![GeneralName::DnsName(
        Ia5String::new("chia.net").map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?,
    )]))
    .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
    let cert = cert
        .build()
        .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
    Ok((
        cert.to_pem(LineEnding::default())
            .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?
            .as_bytes()
            .to_vec(),
        cert_key
            .to_pkcs8_pem(LineEnding::default())
            .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?
            .as_bytes()
            .to_vec(),
    ))
}

pub fn make_ca_cert(cert_path: &Path, key_path: &Path) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let (cert_data, key_data) = make_ca_cert_data()
        .map_err(|e| Error::new(ErrorKind::Other, format!("OpenSSL Errors: {e:?}")))?;
    write_ssl_cert_and_key(cert_path, &cert_data, key_path, &key_data, true)?;
    Ok((cert_data, key_data))
}

fn make_ca_cert_data() -> Result<(Vec<u8>, Vec<u8>), Error> {
    let mut rng = rand::thread_rng();
    let root_key = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("failed to generate a key");
    let pub_key = root_key.to_public_key();
    let signing_key: SigningKey<Sha256> = SigningKey::new(root_key.clone());
    let name = Name::from_str("CN=Chia CA,O=Chia,OU=Organic Farming Division")
        .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
    let subject_pub_key = SubjectPublicKeyInfo::from_pem(
        pub_key
            .to_public_key_pem(LineEnding::default())
            .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?
            .as_bytes(),
    )
    .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
    let cert = CertificateBuilder::new(
        Profile::SubCA {
            issuer: name.clone(),
            path_len_constraint: None,
        },
        SerialNumber::from(rng.gen::<u32>()),
        Validity {
            not_before: Time::UtcTime(
                UtcTime::from_system_time(SystemTime::UNIX_EPOCH)
                    .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?,
            ),
            not_after: Time::UtcTime(
                UtcTime::from_system_time(
                    SystemTime::now().add(Duration::from_secs(60 * 60 * 24 * 3650)),
                )
                .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?,
            ),
        },
        name,
        subject_pub_key,
        &signing_key,
    )
    .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
    let cert = cert
        .build()
        .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
    Ok((
        cert.to_pem(LineEnding::default())
            .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?
            .as_bytes()
            .to_vec(),
        root_key
            .to_pkcs8_pem(LineEnding::default())
            .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?
            .as_bytes()
            .to_vec(),
    ))
}

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
        .map_err(|e| Error::new(ErrorKind::Other, format!("OpenSSL Errors: {e:?}")))?;
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
    private_map.extend(private_certs);
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
    public_map.extend(public_certs);
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
    let (crt, key) = if !private_ca_crt_path.exists() || !private_ca_key_path.exists() || overwrite
    {
        info!("Generating SSL CA Cert");
        make_ca_cert(&private_ca_crt_path, &private_ca_key_path)?
    } else {
        info!("Loading SSL CA Cert");
        (
            fs::read(private_ca_crt_path)?,
            fs::read(private_ca_key_path)?,
        )
    };
    info!("Checking SSL Private Certs");
    generate_ssl_for_nodes(
        ssl_dir,
        &crt,
        &key,
        "private",
        &ALL_PRIVATE_NODE_NAMES,
        overwrite,
    )?;
    info!("Checking SSL Public Certs");
    generate_ssl_for_nodes(
        ssl_dir,
        CHIA_CA_CRT.as_bytes(),
        CHIA_CA_KEY.as_bytes(),
        "public",
        &ALL_PUBLIC_NODE_NAMES,
        overwrite,
    )
}

#[must_use]
pub fn validate_all_ssl(ssl_dir: &Path) -> bool {
    let ca_dir = ssl_dir.join(Path::new("ca"));
    if ca_dir.exists() {
        let private_ca_key_path = ca_dir.join("private_ca.key");
        let private_ca_crt_path = ca_dir.join("private_ca.crt");
        let chia_ca_crt_path = ca_dir.join("chia_ca.crt");
        let chia_ca_key_path = ca_dir.join("chia_ca.key");
        if !validate_cert(&private_ca_crt_path)
            || !validate_cert(&chia_ca_crt_path)
            || !validate_key(&private_ca_key_path)
            || !validate_key(&chia_ca_key_path)
        {
            false
        } else {
            validate_node_paths(ssl_dir, "private", &ALL_PRIVATE_NODE_NAMES)
                && validate_node_paths(ssl_dir, "public", &ALL_PUBLIC_NODE_NAMES)
        }
    } else {
        false
    }
}

fn validate_node_paths(ssl_dir: &Path, prefix: &str, nodes: &[&str]) -> bool {
    for node_name in nodes {
        let node_dir = ssl_dir.join(Path::new(*node_name));
        if !node_dir.exists() {
            return false;
        }
        let crt_path = node_dir.join(Path::new(&format!("{prefix}_{node_name}.crt")));
        let key_path = node_dir.join(Path::new(&format!("{prefix}_{node_name}.key")));
        if key_path.exists() && crt_path.exists() {
            if !validate_cert(&crt_path) || !validate_key(&key_path) {
                return false;
            }
        } else {
            return false;
        }
    }
    true
}

fn validate_cert(path: &Path) -> bool {
    match File::open(path) {
        Ok(cert_file) => {
            let mut reader = BufReader::new(cert_file);
            match certs(&mut reader) {
                Ok(certs) => {
                    for cert in certs.into_iter().map(rustls::Certificate) {
                        if let Err(e) = ParsedCertificate::try_from(&cert) {
                            error!("Error Parsing Cert: {e:?}");
                            return false;
                        }
                    }
                    true
                }
                Err(e) => {
                    error!("Failed to read Cert File: {path:?}, {:?}", e);
                    false
                }
            }
        }
        Err(e) => {
            error!("Failed to read Cert File: {path:?}, {:?}", e);
            false
        }
    }
}

fn validate_key(path: &Path) -> bool {
    match File::open(path) {
        Ok(cert_file) => {
            let mut reader = BufReader::new(cert_file);
            for item in std::iter::from_fn(|| read_one(&mut reader).transpose()) {
                match item {
                    Ok(Item::RSAKey(key)) => {
                        if let Err(e) = rsa::RsaPrivateKey::from_pkcs1_der(&key) {
                            error!("Error Validating Private Key: {path:?}, {e:?}");
                            return false;
                        }
                    }
                    Ok(Item::PKCS8Key(key)) => {
                        if let Err(e) = rsa::RsaPrivateKey::from_pkcs8_der(&key) {
                            error!("Error Validating Private Key: {path:?}, {e:?}");
                            return false;
                        }
                    }
                    Ok(Item::ECKey(_)) => {
                        error!("ECKey is not supported");
                        return false;
                    }
                    Ok(Item::X509Certificate(_)) => error!("Found Certificate, not Private Key"),
                    _ => {
                        error!("Unknown Item while loading private key");
                    }
                }
            }
            true
        }
        Err(e) => {
            error!("Failed to read Cert File: {path:?}, {:?}", e);
            false
        }
    }
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
        if let Err(e) = generate_ca_signed_cert(&crt_path, crt, &key_path, key, overwrite) {
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
        let (cert, key) = generate_ca_signed_cert_data(crt, key)?;
        map.insert((*node_name).to_string(), MemoryNodeSSL { cert, key });
    }
    Ok(map)
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SslCertInfo {
    #[serde(default)]
    pub public_crt: Option<String>,
    #[serde(default)]
    pub public_key: Option<String>,
    pub private_crt: String,
    pub private_key: String,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SslInfo {
    pub root_path: String,
    pub certs: SslCertInfo,
    pub ca: SslCertInfo,
}
