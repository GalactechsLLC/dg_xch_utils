use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind, Read};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub version: u32,
    pub data_dir: PathBuf,
    pub plot_directories: Vec<PathBuf>,
}

pub fn default_paths() -> Result<(PathBuf, PathBuf), Error> {
    if cfg!(target_os = "linux") {
        let home = directories::BaseDirs::new()
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "cannot locate home directory"))?;
        let root = home.home_dir().join(".dgx");
        return Ok((root.join("config"), root.join("data")));
    }
    let paths = ProjectDirs::from("com", "Galactechs", "dg_xch")
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "cannot locate user directories"))?;
    Ok((
        paths.config_dir().to_owned(),
        paths.data_local_dir().to_owned(),
    ))
}

pub fn config_dir(explicit: Option<&Path>) -> Result<PathBuf, Error> {
    match explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("DGX_CONFIG_DIR").map(PathBuf::from))
    {
        Some(path) => std::path::absolute(path),
        None => {
            let preferred = default_paths()?.0;
            if !preferred.join("dgx.json").try_exists()?
                && let Some(legacy) = ProjectDirs::from("com", "Galactechs", "dg_xch")
                && legacy.config_dir().join("dgx.json").try_exists()?
            {
                return Ok(legacy.config_dir().to_owned());
            }
            Ok(preferred)
        }
    }
}

impl AppConfig {
    pub fn ensure(&self, root: &Path) -> Result<(), Error> {
        self.validate()?;
        if root.join("dgx.json").try_exists()? {
            if Self::load(root)? != *self {
                return Err(Error::new(
                    ErrorKind::AlreadyExists,
                    "application profile differs from the requested storage paths",
                ));
            }
            Ok(())
        } else {
            self.save_new(root)
        }
    }

    pub fn load(root: &Path) -> Result<Self, Error> {
        let file = std::fs::File::open(root.join("dgx.json")).map_err(|error| {
            Error::new(
                error.kind(),
                format!(
                    "{}: {error}; run dgx init first (or select --config-dir)",
                    root.display()
                ),
            )
        })?;
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "application configuration exceeds 64 KiB",
            ));
        }
        let config: Self = serde_json::from_slice(&bytes).map_err(Error::other)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1
            || !self.data_dir.is_absolute()
            || self.plot_directories.is_empty()
            || self.plot_directories.iter().any(|path| !path.is_absolute())
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "unsupported application configuration or non-absolute storage paths",
            ));
        }
        Ok(())
    }

    pub fn save_new(&self, root: &Path) -> Result<(), Error> {
        self.validate()?;
        crate::chain_config::write_new(
            &root.join("dgx.json"),
            &serde_json::to_vec_pretty(self).map_err(Error::other)?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paths_use_the_platform_home() {
        let (config, data) = default_paths().unwrap();
        assert!(config.is_absolute());
        assert!(data.is_absolute());
        if cfg!(target_os = "linux") {
            let root = directories::BaseDirs::new()
                .unwrap()
                .home_dir()
                .join(".dgx");
            assert_eq!(config, root.join("config"));
            assert_eq!(data, root.join("data"));
        }
    }

    #[test]
    fn profile_round_trip_and_no_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let config = AppConfig {
            version: 1,
            data_dir: directory.path().join("data"),
            plot_directories: vec![directory.path().join("plots")],
        };
        config.save_new(directory.path()).unwrap();
        assert_eq!(AppConfig::load(directory.path()).unwrap(), config);
        assert!(config.save_new(directory.path()).is_err());
        config.ensure(directory.path()).unwrap();
        let mut changed = config.clone();
        changed.data_dir = directory.path().join("other-data");
        assert_eq!(
            changed.ensure(directory.path()).unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(AppConfig::load(directory.path()).unwrap(), config);
    }

    #[test]
    fn rejects_uninitialized_invalid_and_oversized_profiles() {
        let directory = tempfile::tempdir().unwrap();
        assert!(AppConfig::load(directory.path()).is_err());
        let config = AppConfig {
            version: 1,
            data_dir: PathBuf::from("relative"),
            plot_directories: vec![],
        };
        assert!(config.save_new(directory.path()).is_err());
        std::fs::write(directory.path().join("dgx.json"), vec![b' '; 65537]).unwrap();
        assert!(AppConfig::load(directory.path()).is_err());
    }
}
