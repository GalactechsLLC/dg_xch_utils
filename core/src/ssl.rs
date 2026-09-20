use crate::constants::{ALL_PRIVATE_NODE_NAMES, ALL_PUBLIC_NODE_NAMES, CHIA_CA_CRT, CHIA_CA_KEY};
use der::asn1::{Ia5String, UtcTime};
use der::pem::LineEnding;
use der::{DateTime, EncodePem};
use log::{error, info};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};
use rustls::DigitallySignedStruct;
use rustls::DistinguishedName;
use rustls::SignatureScheme;
use rustls::client::danger::HandshakeSignatureValid;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls_pemfile::{Item, certs, read_one};
use secrecy::zeroize::Zeroizing;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::fs;
use std::fs::{File, OpenOptions, create_dir_all};
use std::io::{BufReader, Error, ErrorKind, Read, Write};
use std::ops::{Add, Sub};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use x509_cert::Certificate;
use x509_cert::builder::{Builder, CertificateBuilder, Profile};
use x509_cert::der::DecodePem;
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::SubjectPublicKeyInfo;
use x509_cert::time::{Time, Validity};

#[derive(Debug)]
pub struct AllowAny {}
impl AllowAny {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {})
    }
}

impl ClientCertVerifier for AllowAny {
    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &peer_signature_algorithms())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &peer_signature_algorithms())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        peer_signature_algorithms().supported_schemes()
    }
}

pub fn peer_signature_algorithms() -> rustls::crypto::WebPkiSupportedAlgorithms {
    rustls::crypto::CryptoProvider::get_default().map_or_else(
        || rustls::crypto::aws_lc_rs::default_provider().signature_verification_algorithms,
        |provider| provider.signature_verification_algorithms,
    )
}

pub fn load_certs(filename: &str) -> Result<Vec<CertificateDer<'static>>, Error> {
    let cert_file = File::open(filename)?;
    let mut reader = BufReader::new(cert_file);
    let mut output = vec![];
    for cert in certs(&mut reader) {
        output.push(cert.map_err(Error::other)?.to_owned());
    }
    Ok(output)
}

pub fn load_certs_from_bytes(bytes: &[u8]) -> Result<Vec<CertificateDer<'static>>, Error> {
    let mut reader = BufReader::new(bytes);
    let mut output = vec![];
    for cert in certs(&mut reader) {
        output.push(cert.map_err(Error::other)?.to_owned());
    }
    Ok(output)
}

pub fn load_private_key(filename: &str) -> Result<PrivateKeyDer<'static>, Error> {
    let key_data = read_ssl_file(Path::new(filename), true)?;
    load_private_key_from_bytes(&key_data)
}

pub fn load_private_key_from_bytes(bytes: &[u8]) -> Result<PrivateKeyDer<'static>, Error> {
    let mut reader = BufReader::new(bytes);
    for item in std::iter::from_fn(|| read_one(&mut reader).transpose()) {
        match item {
            Ok(Item::Pkcs1Key(key)) => {
                return Ok(PrivateKeyDer::from(key.clone_key()));
            }
            Ok(Item::Pkcs8Key(key)) => {
                return Ok(PrivateKeyDer::from(key.clone_key()));
            }
            Ok(Item::Sec1Key(key)) => {
                return Ok(PrivateKeyDer::from(key.clone_key()));
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
    if ssl_pair_exists(cert_path, key_path)? && !overwrite {
        return load_ssl_cert_and_key(cert_path, key_path);
    }
    let (cert_data, key_data) = generate_ca_signed_cert_data(cert_data, key_data)?;
    let key_data = Zeroizing::new(key_data);
    write_ssl_cert_and_key(cert_path, &cert_data, key_path, &key_data, overwrite)?;
    Ok((cert_data, key_data.to_vec()))
}

fn ssl_pair_exists(cert_path: &Path, key_path: &Path) -> Result<bool, Error> {
    if cert_path == key_path {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "TLS certificate and private key paths must differ",
        ));
    }
    let exists = |path: &Path| -> Result<bool, Error> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
            Ok(_) => Err(Error::new(
                ErrorKind::InvalidInput,
                format!("TLS identity is not a regular file: {}", path.display()),
            )),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    };
    match (exists(cert_path)?, exists(key_path)?) {
        (true, true) => Ok(true),
        (false, false) => Ok(false),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            "incomplete TLS certificate/private-key pair; restore both files before continuing",
        )),
    }
}

fn read_ssl_file(path: &Path, private: bool) -> Result<Zeroizing<Vec<u8>>, Error> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "TLS identity must be a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if private && (metadata.mode() & 0o077 != 0 || metadata.nlink() != 1) {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "TLS private key must have owner-only permissions and no hard links: {}",
                    path.display()
                ),
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    let limit = 1024 * 1024;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit as usize {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "TLS identity file exceeds 1 MiB",
        ));
    }
    Ok(bytes)
}

fn validate_ssl_cert_and_key(cert_data: &[u8], key_data: &[u8]) -> Result<(), Error> {
    let certificates = load_certs_from_bytes(cert_data)?;
    let private_key = load_private_key_from_bytes(key_data)?;
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    rustls::sign::CertifiedKey::from_der(certificates, private_key, &provider)
        .and_then(|identity| identity.keys_match())
        .map_err(|error| Error::new(ErrorKind::InvalidData, error))
}

pub fn load_ssl_cert_and_key(
    cert_path: &Path,
    key_path: &Path,
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let cert_data = read_ssl_file(cert_path, false)?;
    let key_data = read_ssl_file(key_path, true)?;
    validate_ssl_cert_and_key(&cert_data, &key_data)?;
    Ok((cert_data.to_vec(), key_data.to_vec()))
}

struct StagedSslFile {
    path: PathBuf,
}

impl StagedSslFile {
    fn new(destination: &Path, bytes: &[u8]) -> Result<Self, Error> {
        let filename = destination.file_name().ok_or_else(|| {
            Error::new(ErrorKind::InvalidInput, "TLS destination has no filename")
        })?;
        let mut staged_name = filename.to_os_string();
        staged_name.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let path = destination.with_file_name(staged_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        let staged = Self { path };
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(staged)
    }
}

impl Drop for StagedSslFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn write_ssl_cert_and_key(
    cert_path: &Path,
    cert_data: &[u8],
    key_path: &Path,
    key_data: &[u8],
    overwrite: bool,
) -> Result<(), Error> {
    let existing_key = if ssl_pair_exists(cert_path, key_path)? {
        let (_, existing_key) = load_ssl_cert_and_key(cert_path, key_path)?;
        let existing_key = Zeroizing::new(existing_key);
        if !overwrite {
            return Ok(());
        }
        Some(existing_key)
    } else {
        None
    };
    validate_ssl_cert_and_key(cert_data, key_data)?;
    let staged_cert = StagedSslFile::new(cert_path, cert_data)?;
    let staged_key = StagedSslFile::new(key_path, key_data)?;
    let rollback_key = existing_key
        .as_ref()
        .map(|bytes| StagedSslFile::new(key_path, bytes))
        .transpose()?;
    if existing_key.is_none() {
        fs::hard_link(&staged_key.path, key_path)?;
        if let Err(error) = fs::hard_link(&staged_cert.path, cert_path) {
            fs::remove_file(key_path)?;
            return Err(error);
        }
        return Ok(());
    }
    fs::rename(&staged_key.path, key_path)?;
    if let Err(error) = fs::rename(&staged_cert.path, cert_path) {
        let rollback = match rollback_key {
            Some(previous) => fs::rename(&previous.path, key_path),
            None => fs::remove_file(key_path),
        };
        rollback.map_err(|rollback_error| {
            Error::other(format!("TLS certificate write failed: {error}; private key rollback failed: {rollback_error}"))
        })?;
        return Err(error);
    }
    Ok(())
}

pub fn generate_ca_signed_cert_data(
    cert_data: &[u8],
    key_data: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    generate_ca_signed_cert_data_for_host(cert_data, key_data, "chia.net")
}

pub fn generate_ca_signed_cert_data_for_host(
    cert_data: &[u8],
    key_data: &[u8],
    host: &str,
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    validate_ssl_cert_and_key(cert_data, key_data)?;
    let root_cert = Certificate::from_pem(cert_data).map_err(|e| Error::other(format!("{e:?}")))?;
    let root_key = rsa::RsaPrivateKey::from_pkcs1_pem(&String::from_utf8_lossy(key_data))
        .or_else(|_| rsa::RsaPrivateKey::from_pkcs8_pem(&String::from_utf8_lossy(key_data)))
        .map_err(|e| Error::other(format!("Failed to load Root Key: {e:?}")))?;
    use rsa::rand_core::RngCore;
    let mut rng = rsa::rand_core::OsRng;
    let cert_key =
        rsa::RsaPrivateKey::new(&mut rng, 2048).map_err(|e| Error::other(format!("{e:?}")))?;
    let pub_key = cert_key.to_public_key();
    let signing_key: SigningKey<Sha256> = SigningKey::new(root_key);
    let subject_pub_key = SubjectPublicKeyInfo::from_pem(
        pub_key
            .to_public_key_pem(LineEnding::default())
            .map_err(|e| Error::other(format!("{e:?}")))?
            .as_bytes(),
    )
    .map_err(|e| Error::other(format!("{e:?}")))?;
    let mut cert = CertificateBuilder::new(
        Profile::Leaf {
            issuer: root_cert.tbs_certificate.issuer,
            enable_key_agreement: false,
            enable_key_encipherment: false,
        },
        SerialNumber::from(rng.next_u32()),
        Validity {
            not_before: Time::UtcTime(
                UtcTime::from_system_time(SystemTime::now().sub(Duration::from_secs(60 * 60 * 24)))
                    .map_err(|e| Error::other(format!("{e:?}")))?,
            ),
            not_after: Time::UtcTime(
                UtcTime::from_date_time(
                    DateTime::new(2049, 8, 2, 0, 0, 0)
                        .map_err(|e| Error::other(format!("{e:?}")))?,
                )
                .map_err(|e| Error::other(format!("{e:?}")))?,
            ),
        },
        Name::from_str("CN=Chia,O=Chia,OU=Organic Farming Division")
            .map_err(|e| Error::other(format!("{e:?}")))?,
        subject_pub_key,
        &signing_key,
    )
    .map_err(|e| Error::other(format!("{e:?}")))?;
    let mut names = vec![GeneralName::DnsName(
        Ia5String::new(host).map_err(|error| Error::other(format!("{error:?}")))?,
    )];
    if host == "localhost" {
        names.push(GeneralName::IpAddress(
            der::asn1::OctetString::new([127, 0, 0, 1]).map_err(Error::other)?,
        ));
        names.push(GeneralName::IpAddress(
            der::asn1::OctetString::new(std::net::Ipv6Addr::LOCALHOST.octets())
                .map_err(Error::other)?,
        ));
    }
    cert.add_extension(&SubjectAltName(names))
        .map_err(|e| Error::other(format!("{e:?}")))?;
    let cert = cert.build().map_err(|e| Error::other(format!("{e:?}")))?;
    Ok((
        cert.to_pem(LineEnding::default())
            .map_err(|e| Error::other(format!("{e:?}")))?
            .as_bytes()
            .to_vec(),
        cert_key
            .to_pkcs8_pem(LineEnding::default())
            .map_err(|e| Error::other(format!("{e:?}")))?
            .as_bytes()
            .to_vec(),
    ))
}

pub fn make_ca_cert(cert_path: &Path, key_path: &Path) -> Result<(Vec<u8>, Vec<u8>), Error> {
    ssl_pair_exists(cert_path, key_path)?;
    let (cert_data, key_data) =
        make_ca_cert_data().map_err(|e| Error::other(format!("OpenSSL Errors: {e:?}")))?;
    let key_data = Zeroizing::new(key_data);
    write_ssl_cert_and_key(cert_path, &cert_data, key_path, &key_data, true)?;
    Ok((cert_data, key_data.to_vec()))
}

pub fn make_ca_cert_data() -> Result<(Vec<u8>, Vec<u8>), Error> {
    use rsa::rand_core::RngCore;
    let mut rng = rsa::rand_core::OsRng;
    let root_key = rsa::RsaPrivateKey::new(&mut rng, 2048)
        .map_err(|error| Error::other(format!("failed to generate CA key: {error}")))?;
    let pub_key = root_key.to_public_key();
    let signing_key: SigningKey<Sha256> = SigningKey::new(root_key.clone());
    let name = Name::from_str("CN=Chia CA,O=Chia,OU=Organic Farming Division")
        .map_err(|e| Error::other(format!("{e:?}")))?;
    let subject_pub_key = SubjectPublicKeyInfo::from_pem(
        pub_key
            .to_public_key_pem(LineEnding::default())
            .map_err(|e| Error::other(format!("{e:?}")))?
            .as_bytes(),
    )
    .map_err(|e| Error::other(format!("{e:?}")))?;
    let cert = CertificateBuilder::new(
        Profile::SubCA {
            issuer: name.clone(),
            path_len_constraint: None,
        },
        SerialNumber::from(rng.next_u32()),
        Validity {
            not_before: Time::UtcTime(
                UtcTime::from_system_time(SystemTime::UNIX_EPOCH)
                    .map_err(|e| Error::other(format!("{e:?}")))?,
            ),
            not_after: Time::UtcTime(
                UtcTime::from_system_time(
                    SystemTime::now().add(Duration::from_secs(60 * 60 * 24 * 3650)),
                )
                .map_err(|e| Error::other(format!("{e:?}")))?,
            ),
        },
        name,
        subject_pub_key,
        &signing_key,
    )
    .map_err(|e| Error::other(format!("{e:?}")))?;
    let cert = cert.build().map_err(|e| Error::other(format!("{e:?}")))?;
    Ok((
        cert.to_pem(LineEnding::default())
            .map_err(|e| Error::other(format!("{e:?}")))?
            .as_bytes()
            .to_vec(),
        root_key
            .to_pkcs8_pem(LineEnding::default())
            .map_err(|e| Error::other(format!("{e:?}")))?
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
    let (ca_cert_data, ca_key_data) =
        make_ca_cert_data().map_err(|e| Error::other(format!("OpenSSL Errors: {e:?}")))?;
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
    let private_ca_exists = ssl_pair_exists(&private_ca_crt_path, &private_ca_key_path)?;
    if !private_ca_exists {
        for node_name in ALL_PRIVATE_NODE_NAMES {
            let node_dir = ssl_dir.join(node_name);
            for extension in ["crt", "key"] {
                let path = node_dir.join(format!("private_{node_name}.{extension}"));
                match fs::symlink_metadata(path) {
                    Ok(_) => {
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            "private TLS identities exist without their CA; restore the original CA",
                        ));
                    }
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    let (crt, key) = if !private_ca_exists || overwrite {
        info!("Generating SSL CA Cert");
        make_ca_cert(&private_ca_crt_path, &private_ca_key_path)?
    } else {
        info!("Loading SSL CA Cert");
        load_ssl_cert_and_key(&private_ca_crt_path, &private_ca_key_path)?
    };
    let key = Zeroizing::new(key);
    write_ssl_cert_and_key(
        &chia_ca_crt_path,
        CHIA_CA_CRT.as_bytes(),
        &chia_ca_key_path,
        CHIA_CA_KEY.as_bytes(),
        overwrite,
    )?;
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
        if load_ssl_cert_and_key(&private_ca_crt_path, &private_ca_key_path).is_err()
            || load_ssl_cert_and_key(&chia_ca_crt_path, &chia_ca_key_path).is_err()
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
        if load_ssl_cert_and_key(&crt_path, &key_path).is_err() {
            return false;
        }
    }
    true
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
        if ssl_pair_exists(&crt_path, &key_path)? && !overwrite {
            load_ssl_cert_and_key(&crt_path, &key_path)?;
            continue;
        }
        generate_ca_signed_cert(&crt_path, crt, &key_path, key, overwrite)?;
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

#[cfg(test)]
mod private_file_tests {
    use super::{generate_ca_signed_cert, load_ssl_cert_and_key, write_ssl_cert_and_key};
    use crate::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
    use std::io::ErrorKind;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_test_pair(directory: &TestDirectory) {
        write_ssl_cert_and_key(
            &directory.0.join("test.crt"),
            CHIA_CA_CRT.as_bytes(),
            &directory.0.join("test.key"),
            CHIA_CA_KEY.as_bytes(),
            false,
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_key_creation_does_not_depend_on_umask() {
        let directory = TestDirectory::new();
        write_test_pair(&directory);
        let private_key = directory.0.join("test.key");
        let mode = std::fs::metadata(&private_key)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 2);
    }

    #[test]
    fn existing_identity_is_reused_without_generating_an_unrelated_key() {
        let directory = TestDirectory::new();
        write_test_pair(&directory);
        let (certificate, private_key) = generate_ca_signed_cert(
            &directory.0.join("test.crt"),
            b"unused CA certificate",
            &directory.0.join("test.key"),
            b"unused CA key",
            false,
        )
        .unwrap();
        assert_eq!(certificate, CHIA_CA_CRT.as_bytes());
        assert_eq!(private_key, CHIA_CA_KEY.as_bytes());
    }

    #[test]
    fn partial_identity_is_not_modified_even_when_overwriting() {
        for overwrite in [false, true] {
            let directory = TestDirectory::new();
            let certificate = directory.0.join("test.crt");
            let private_key = directory.0.join("test.key");
            std::fs::write(&certificate, b"preserved certificate").unwrap();
            let error = write_ssl_cert_and_key(
                &certificate,
                CHIA_CA_CRT.as_bytes(),
                &private_key,
                CHIA_CA_KEY.as_bytes(),
                overwrite,
            )
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidData);
            assert_eq!(
                std::fs::read(certificate).unwrap(),
                b"preserved certificate"
            );
            assert!(!private_key.exists());
        }
    }

    #[test]
    fn invalid_pair_is_not_written_and_empty_files_are_rejected() {
        let directory = TestDirectory::new();
        let certificate = directory.0.join("test.crt");
        let private_key = directory.0.join("test.key");
        assert!(write_ssl_cert_and_key(&certificate, b"", &private_key, b"", false).is_err());
        assert!(!certificate.exists());
        assert!(!private_key.exists());
        write_test_pair(&directory);
        std::fs::write(&certificate, b"").unwrap();
        assert!(load_ssl_cert_and_key(&certificate, &private_key).is_err());
    }

    #[test]
    fn mismatched_private_key_is_not_written() {
        let directory = TestDirectory::new();
        let certificate = directory.0.join("test.crt");
        let private_key = directory.0.join("test.key");
        let (_, unrelated_key) = super::make_ca_cert_data().unwrap();
        let error = write_ssl_cert_and_key(
            &certificate,
            CHIA_CA_CRT.as_bytes(),
            &private_key,
            &unrelated_key,
            false,
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(!certificate.exists());
        assert!(!private_key.exists());
    }

    #[cfg(unix)]
    #[test]
    fn permissive_existing_key_is_rejected_without_changing_permissions() {
        let directory = TestDirectory::new();
        write_test_pair(&directory);
        let certificate = directory.0.join("test.crt");
        let private_key = directory.0.join("test.key");
        std::fs::set_permissions(&private_key, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = load_ssl_cert_and_key(&certificate, &private_key).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(
            std::fs::metadata(private_key).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_private_key_is_rejected_without_modifying_its_target() {
        let directory = TestDirectory::new();
        write_test_pair(&directory);
        let certificate = directory.0.join("test.crt");
        let private_key = directory.0.join("test.key");
        let original_key = directory.0.join("original.key");
        std::fs::rename(&private_key, &original_key).unwrap();
        std::os::unix::fs::symlink(&original_key, &private_key).unwrap();
        assert!(load_ssl_cert_and_key(&certificate, &private_key).is_err());
        assert!(
            write_ssl_cert_and_key(
                &certificate,
                CHIA_CA_CRT.as_bytes(),
                &private_key,
                CHIA_CA_KEY.as_bytes(),
                true,
            )
            .is_err()
        );
        assert_eq!(std::fs::read(original_key).unwrap(), CHIA_CA_KEY.as_bytes());
    }

    #[test]
    fn generation_errors_are_returned_to_the_caller() {
        let directory = TestDirectory::new();
        let node_directory = directory.0.join("farmer");
        std::fs::create_dir(&node_directory).unwrap();
        std::fs::write(node_directory.join("private_farmer.crt"), b"partial").unwrap();
        let error = super::generate_ssl_for_nodes(
            &directory.0,
            CHIA_CA_CRT.as_bytes(),
            CHIA_CA_KEY.as_bytes(),
            "private",
            &["farmer"],
            false,
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }
}
