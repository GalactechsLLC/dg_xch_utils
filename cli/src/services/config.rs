use serde::de::DeserializeOwned;
use std::io::{Error, ErrorKind, Read};
use std::path::Path;

pub(crate) const ENV_HELP: &str =
    "Supply the complete configuration through --config <file> or an environment variable:
DGX_FARMER_CONFIG: YAML (JSON also accepted)
DGX_INTRODUCER_CONFIG, DGX_TIMELORD_CONFIG, DGX_POOL_CONFIG: JSON
The environment variable contains the document itself, not a file path.
An explicit --config file takes precedence over the environment variable.";

pub(crate) enum Format {
    Json,
    Yaml,
}

pub(crate) fn load<T: DeserializeOwned>(
    service: &str,
    file: Option<&Path>,
    format: Format,
    limit: usize,
) -> Result<T, Error> {
    // Do not even read environment content when the caller explicitly selects a file.
    let contents = if file.is_none() {
        std::env::var(format!("DGX_{service}_CONFIG"))
            .map(Some)
            .or_else(|error| match error {
                std::env::VarError::NotPresent => Ok(None),
                std::env::VarError::NotUnicode(_) => Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("DGX_{service}_CONFIG must contain UTF-8 configuration text"),
                )),
            })?
    } else {
        None
    };
    load_contents(service, file, contents.as_deref(), format, limit)
}

fn load_contents<T: DeserializeOwned>(
    service: &str,
    file: Option<&Path>,
    contents: Option<&str>,
    format: Format,
    limit: usize,
) -> Result<T, Error> {
    let mut bytes = Vec::new();
    let source = if let Some(file) = file {
        std::fs::File::open(file)?
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)?;
        bytes.as_slice()
    } else if let Some(contents) = contents {
        contents.as_bytes()
    } else {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("supply --config <file> or the complete configuration in DGX_{service}_CONFIG"),
        ));
    };
    if source.len() > limit {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "service configuration exceeds size limit",
        ));
    }
    let parsed = match format {
        Format::Json => serde_json::from_slice(source).map_err(|_| ()),
        Format::Yaml => serde_yaml::from_slice(source).map_err(|_| ()),
    };
    parsed.map_err(|_| {
        // Parser diagnostics can include secret values from the document.
        Error::new(
            ErrorKind::InvalidInput,
            format!(
                "invalid {service} configuration; check the document format and required fields"
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Settings {
        host: String,
        port: u16,
        keys: Vec<String>,
    }

    #[test]
    fn loads_complete_json_or_multiline_yaml_from_environment_contents() {
        for (format, contents) in [
            (
                Format::Json,
                r#"{"host":"node","port":8444,"keys":["111111111111111111"]}"#,
            ),
            (
                Format::Yaml,
                "host: node\nport: 8444\nkeys:\n  - '111111111111111111'\n",
            ),
        ] {
            let settings: Settings =
                load_contents("TEST", None, Some(contents), format, 4096).unwrap();
            assert_eq!(
                settings,
                Settings {
                    host: "node".into(),
                    port: 8444,
                    keys: vec!["111111111111111111".into()]
                }
            );
        }
    }

    #[test]
    fn explicit_file_overrides_environment_and_bad_files_do_not_fall_back() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("config.json");
        std::fs::write(&file, r#"{"host":"file","port":8444,"keys":[]}"#).unwrap();
        let settings: Settings =
            load_contents("TEST", Some(&file), Some("invalid env"), Format::Json, 4096).unwrap();
        assert_eq!(settings.host, "file");
        assert!(
            load_contents::<Settings>(
                "TEST",
                Some(&directory.path().join("missing")),
                Some(r#"{"host":"env","port":8444,"keys":[]}"#),
                Format::Json,
                4096
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_missing_empty_invalid_and_oversized_documents_without_leaking_secrets() {
        assert!(
            load_contents::<Settings>("TEST", None, None, Format::Json, 4096)
                .unwrap_err()
                .to_string()
                .contains("DGX_TEST_CONFIG")
        );
        for contents in [
            "",
            "{sensitive-secret",
            r#"{"host":"sensitive-secret","port":"sensitive-secret","keys":[]}"#,
        ] {
            let error = load_contents::<Settings>("TEST", None, Some(contents), Format::Json, 4096)
                .unwrap_err();
            assert!(!error.to_string().contains("sensitive-secret"));
        }
        assert!(load_contents::<Settings>("TEST", None, Some("too big"), Format::Json, 2).is_err());
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("config.json");
        std::fs::write(&file, "too big").unwrap();
        assert!(load_contents::<Settings>("TEST", Some(&file), None, Format::Json, 2).is_err());
    }
}
