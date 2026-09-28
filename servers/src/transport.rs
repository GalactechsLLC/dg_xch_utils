use dg_xch_core::protocols::shared::{CAPABILITIES, Handshake};
use dg_xch_core::protocols::{ChiaMessage, NodeType, ProtocolMessageTypes};
use dg_xch_core::ssl::{load_certs, load_private_key};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Error, ErrorKind};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;

pub const SERVICE_MESSAGE_LIMIT: usize = 64 * 1024;

pub async fn shutdown_signal() -> Result<(), Error> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsIdentity {
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub ca_certificate: PathBuf,
}

impl TlsIdentity {
    fn roots(&self) -> Result<RootCertStore, Error> {
        let mut roots = RootCertStore::empty();
        for certificate in load_certs(path_string(&self.ca_certificate)?)? {
            roots.add(certificate).map_err(Error::other)?;
        }
        if roots.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "TLS trust store is empty",
            ));
        }
        Ok(roots)
    }

    pub fn client(&self) -> Result<Arc<ClientConfig>, Error> {
        Ok(Arc::new(
            ClientConfig::builder()
                .with_root_certificates(self.roots()?)
                .with_client_auth_cert(
                    load_certs(path_string(&self.certificate)?)?,
                    load_private_key(path_string(&self.private_key)?)?,
                )
                .map_err(Error::other)?,
        ))
    }

    pub fn public_server(&self) -> Result<Arc<ServerConfig>, Error> {
        Ok(Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    load_certs(path_string(&self.certificate)?)?,
                    load_private_key(path_string(&self.private_key)?)?,
                )
                .map_err(Error::other)?,
        ))
    }
}

fn path_string(path: &std::path::Path) -> Result<&str, Error> {
    path.to_str()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "TLS path is not UTF-8"))
}

pub fn decode_exact<T: ChiaSerialize>(bytes: &[u8]) -> Result<T, Error> {
    if bytes.len() > SERVICE_MESSAGE_LIMIT {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "service message exceeds limit",
        ));
    }
    let mut cursor = Cursor::new(bytes);
    let value = T::from_bytes(&mut cursor, ChiaProtocolVersion::default())?;
    if cursor.position() != bytes.len() as u64 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "trailing bytes in service message",
        ));
    }
    Ok(value)
}

pub fn encode<T: ChiaSerialize>(
    kind: ProtocolMessageTypes,
    value: &T,
    id: Option<u16>,
) -> Result<Message, Error> {
    let version = ChiaProtocolVersion::default();
    let bytes = ChiaMessage::new(kind, version, value, id)?.to_bytes(version)?;
    if bytes.len() > SERVICE_MESSAGE_LIMIT {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "service response exceeds limit",
        ));
    }
    Ok(Message::Binary(bytes.into()))
}

pub fn handshake(network_id: &str, node_type: NodeType, server_port: u16) -> Handshake {
    Handshake {
        network_id: network_id.to_owned(),
        protocol_version: ChiaProtocolVersion::default().to_string(),
        software_version: format!("dg_xch/{}", env!("CARGO_PKG_VERSION")),
        server_port,
        node_type: node_type as u8,
        capabilities: CAPABILITIES
            .iter()
            .map(|entry| (entry.0, entry.1.to_owned()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_rejects_trailing_and_oversized_data() {
        let greeting = handshake("test", NodeType::Introducer, 8444);
        let mut bytes = greeting.to_bytes(ChiaProtocolVersion::default()).unwrap();
        assert!(decode_exact::<Handshake>(&bytes).is_ok());
        bytes.push(0);
        assert!(decode_exact::<Handshake>(&bytes).is_err());
        assert!(decode_exact::<Handshake>(&vec![0; SERVICE_MESSAGE_LIMIT + 1]).is_err());
    }
}
