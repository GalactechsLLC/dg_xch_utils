use dg_xch_core::consensus::chain_definition::{ChainSelection, ResolvedChain};
use dg_xch_core::ssl::create_all_ssl;
use std::io::{Error, ErrorKind, Read, Write};
use std::path::Path;

const CONFIG_LIMIT: u64 = 64 * 1024;

pub fn read_selection(path: &Path) -> Result<ChainSelection, Error> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(CONFIG_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > CONFIG_LIMIT {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "chain configuration exceeds 64 KiB",
        ));
    }
    let selection: ChainSelection = serde_json::from_slice(&bytes).map_err(Error::other)?;
    selection.resolve().map_err(Error::other)?;
    Ok(selection)
}

pub fn write_new(path: &Path, data: &[u8]) -> Result<(), Error> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(data)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn ensure_selection(path: &Path, selection: &ChainSelection) -> Result<ResolvedChain, Error> {
    let requested = selection.resolve().map_err(Error::other)?;
    match read_selection(path) {
        Ok(existing) => {
            let existing = existing.resolve().map_err(Error::other)?;
            if existing.constants != requested.constants
                || existing.handshake_network_id != requested.handshake_network_id
            {
                return Err(Error::new(
                    ErrorKind::AlreadyExists,
                    "chain configuration already exists with different rules; use a separate directory",
                ));
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            write_new(
                path,
                &serde_json::to_vec_pretty(selection).map_err(Error::other)?,
            )?;
        }
        Err(error) => return Err(error),
    }
    Ok(requested)
}

pub fn initialize(root: &Path, selection: &ChainSelection) -> Result<ResolvedChain, Error> {
    let manifest = root.join("chain.json");
    if !manifest.try_exists()?
        && root.try_exists()?
        && std::fs::read_dir(root)?.next().transpose()?.is_some()
    {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            "initialization requires an empty directory or an existing matching chain.json",
        ));
    }
    let chain = ensure_selection(&manifest, selection)?;
    create_all_ssl(&root.join("ssl"), false)?;
    std::fs::create_dir_all(root.join("data"))?;
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::consensus::chain_definition::ChainDefinition;

    #[test]
    fn initialization_is_repeatable_and_rejects_cross_chain_reuse() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("chain");
        let first = initialize(&root, &ChainSelection::default()).unwrap();
        let key = std::fs::read(root.join("ssl/ca/private_ca.key")).unwrap();
        let second = initialize(&root, &ChainSelection::default()).unwrap();
        assert_eq!(first.constants, second.constants);
        assert_eq!(
            key,
            std::fs::read(root.join("ssl/ca/private_ca.key")).unwrap()
        );
        assert!(initialize(&root, &ChainSelection::Dgx).is_err());
    }

    #[test]
    fn existing_files_are_not_adopted_or_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let existing = directory.path().join("important");
        std::fs::write(&existing, b"preserve").unwrap();
        assert!(initialize(directory.path(), &ChainSelection::default()).is_err());
        assert!(write_new(&existing, b"replace").is_err());
        assert_eq!(std::fs::read(existing).unwrap(), b"preserve");
    }

    #[test]
    fn custom_configuration_is_validated_and_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("chain.json");
        let selection = ChainSelection::Custom(ChainDefinition::development("fixture".into()));
        ensure_selection(&path, &selection).unwrap();
        assert_eq!(read_selection(&path).unwrap(), selection);
        std::fs::write(&path, vec![b' '; CONFIG_LIMIT as usize + 1]).unwrap();
        assert!(read_selection(&path).is_err());
    }
}
