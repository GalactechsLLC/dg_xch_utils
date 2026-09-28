use dg_xch_core::plots::PlotHeader;
use dg_xch_pos::plots::plot_reader::read_plot_file_header_async;
use log::warn;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Error, ErrorKind, Read};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlotFormat {
    Pos1,
    Pos2,
    Bladebit,
    Gigahorse,
}

pub fn detect_plot_format(mut input: impl Read) -> Result<PlotFormat, Error> {
    let mut prefix = [0; 54];
    input.read_exact(&mut prefix[..4])?;
    match &prefix[..4] {
        b"pos2" => return Ok(PlotFormat::Pos2),
        b"PLOT" => return Ok(PlotFormat::Bladebit),
        _ => {}
    }
    input.read_exact(&mut prefix[4..])?;
    if &prefix[..19] != b"Proof of Space Plot" {
        return Err(Error::new(ErrorKind::InvalidData, "Unknown plot format"));
    }
    let length = usize::from(u16::from_be_bytes([prefix[52], prefix[53]]));
    if length == 0 || length > 256 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Invalid plot format description",
        ));
    }
    let mut description = vec![0; length];
    input.read_exact(&mut description)?;
    if description.starts_with(b"mmx-") {
        Ok(PlotFormat::Gigahorse)
    } else if description == b"v1.0" {
        Ok(PlotFormat::Pos1)
    } else {
        Err(Error::new(
            ErrorKind::Unsupported,
            "Unsupported plot format description",
        ))
    }
}

#[derive(Clone, Default)]
pub(crate) struct PlotInventory {
    entries: HashMap<PathBuf, PlotFormat>,
}

impl PlotInventory {
    pub(crate) async fn scan(directories: Vec<PathBuf>) -> Result<Self, Error> {
        tokio::task::spawn_blocking(move || {
            let mut inventory = Self::default();
            let mut visited = HashSet::new();
            for directory in directories {
                let directory = match directory.canonicalize() {
                    Ok(directory) => directory,
                    Err(error) => {
                        warn!(
                            "Cannot scan plot directory {}: {error}",
                            directory.display()
                        );
                        continue;
                    }
                };
                if !visited.insert(directory.clone()) {
                    continue;
                }
                let entries = match std::fs::read_dir(&directory) {
                    Ok(entries) => entries,
                    Err(error) => {
                        warn!(
                            "Cannot scan plot directory {}: {error}",
                            directory.display()
                        );
                        continue;
                    }
                };
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => {
                            warn!("Cannot read plot directory entry: {error}");
                            continue;
                        }
                    };
                    let path = entry.path();
                    if path.extension().is_none_or(|extension| extension != "plot")
                        || !path.is_file()
                    {
                        continue;
                    }
                    let detected = path.canonicalize().and_then(|path| {
                        File::open(&path)
                            .and_then(detect_plot_format)
                            .map(|format| (path, format))
                    });
                    match detected {
                        Ok((path, format)) => {
                            inventory.entries.insert(path, format);
                        }
                        Err(error) => warn!("Plot rejected {}: {error}", path.display()),
                    }
                }
            }
            inventory
        })
        .await
        .map_err(Error::other)
    }

    pub(crate) fn paths(&self, format: PlotFormat) -> Vec<PathBuf> {
        let mut paths: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, detected)| **detected == format)
            .map(|(path, _)| path.clone())
            .collect();
        paths.sort();
        paths
    }

    pub(crate) fn classic_paths(&self) -> Vec<PathBuf> {
        let mut paths = self.paths(PlotFormat::Pos1);
        paths.extend(self.paths(PlotFormat::Bladebit));
        paths
    }
}

pub(crate) async fn read_classic_headers(
    paths: &[PathBuf],
    existing: &[PathBuf],
) -> Result<(HashMap<PathBuf, PlotHeader>, HashSet<PathBuf>), Error> {
    let existing: HashSet<&Path> = existing.iter().map(PathBuf::as_path).collect();
    let mut headers = HashMap::new();
    let mut failed = HashSet::new();
    for path in paths {
        if existing.contains(path.as_path()) {
            continue;
        }
        match read_plot_file_header_async(path).await {
            Ok((path, header)) => {
                headers.insert(path, header);
            }
            Err(error) => {
                warn!("Plot header rejected {}: {error}", path.display());
                failed.insert(path.clone());
            }
        }
    }
    Ok((headers, failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_header(description: &[u8]) -> Vec<u8> {
        let mut header = vec![0; 54];
        header[..19].copy_from_slice(b"Proof of Space Plot");
        header[52..54].copy_from_slice(&(description.len() as u16).to_be_bytes());
        header.extend_from_slice(description);
        header
    }

    #[test]
    fn routes_by_header_not_filename_or_backend_success() {
        for (header, expected) in [
            (legacy_header(b"v1.0"), PlotFormat::Pos1),
            (b"pos2".to_vec(), PlotFormat::Pos2),
            (b"PLOT".to_vec(), PlotFormat::Bladebit),
            (legacy_header(b"mmx-v3.0"), PlotFormat::Gigahorse),
            (legacy_header(b"mmx-v2.5"), PlotFormat::Gigahorse),
            (legacy_header(b"mmx-v9.9"), PlotFormat::Gigahorse),
        ] {
            assert_eq!(detect_plot_format(header.as_slice()).unwrap(), expected);
        }
        assert!(detect_plot_format(legacy_header(b"unknown").as_slice()).is_err());
        assert!(detect_plot_format(&b"Proof"[..]).is_err());
        assert!(detect_plot_format(legacy_header(b"").as_slice()).is_err());
    }

    #[tokio::test]
    async fn mixed_directory_is_deduplicated_and_never_sends_gigahorse_to_pos1() {
        let directory = tempfile::tempdir().unwrap();
        for (name, header) in [
            ("classic.plot", legacy_header(b"v1.0")),
            ("compressed.plot", b"PLOT".to_vec()),
            ("next.plot", b"pos2".to_vec()),
            ("unsupported.plot", legacy_header(b"mmx-v2.5")),
            ("c30.plot", legacy_header(b"mmx-v3.0")),
            ("bad.plot", vec![0; 54]),
        ] {
            std::fs::write(directory.path().join(name), header).unwrap();
        }
        let inventory = PlotInventory::scan(vec![
            directory.path().to_owned(),
            directory.path().join("."),
        ])
        .await
        .unwrap();
        assert_eq!(inventory.entries.len(), 5);
        assert_eq!(inventory.classic_paths().len(), 2);
        assert_eq!(inventory.paths(PlotFormat::Gigahorse).len(), 2);
        std::fs::remove_file(directory.path().join("classic.plot")).unwrap();
        let refreshed = PlotInventory::scan(vec![directory.path().to_owned()])
            .await
            .unwrap();
        assert_eq!(refreshed.classic_paths().len(), 1);
    }
}
