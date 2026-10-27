use crate::app::Desktop;
use crate::backend::Backend;
use crate::config::{AppPaths, Settings};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub fn run(
    arguments: &[std::ffi::OsString],
    config_root: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let smoke = match arguments {
        [] => false,
        [argument] if argument == "--smoke-test" => true,
        _ => return Err("usage: dgx gui [--smoke-test]".into()),
    };
    let temporary = smoke.then(tempfile::tempdir).transpose()?;
    let paths = if let Some(directory) = &temporary {
        AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
        }
    } else {
        AppPaths {
            config: config_root.to_path_buf(),
            data: dg_xch_servers::app_config::AppConfig::load(config_root)?.data_dir,
        }
    };
    let settings = if smoke {
        smoke_settings(
            &paths,
            &std::env::var("DGX_GUI_SMOKE_PROFILE").unwrap_or_else(|_| "bls".into()),
        )?
    } else {
        paths.load()?
    };
    let backend = if smoke {
        Backend::for_smoke_test(paths.clone(), settings.clone())?
    } else {
        Backend::new(paths.clone(), settings.clone())?
    };
    let completed = Arc::new(AtomicBool::new(false));
    let completion = completed.clone();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Druid Garden")
            .with_inner_size([1280.0, 840.0])
            .with_min_inner_size([900.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Druid Garden",
        options,
        Box::new(move |context| {
            let desktop = Desktop::new(context, paths, settings, backend);
            Ok(Box::new(if smoke {
                desktop.with_smoke_test(completion)
            } else {
                desktop
            }))
        }),
    )?;
    if smoke && !completed.load(Ordering::Acquire) {
        return Err("native desktop closed before all pages rendered".into());
    }
    Ok(())
}

fn smoke_settings(paths: &AppPaths, profile: &str) -> Result<Settings, Box<dyn std::error::Error>> {
    if !matches!(profile, "bls" | "falcon") {
        return Err("DGX_GUI_SMOKE_PROFILE must be bls or falcon".into());
    }
    let mut settings = Settings::default();
    if profile == "falcon" {
        if !cfg!(feature = "falcon") {
            return Err("Falcon smoke testing requires the falcon feature".into());
        }
        std::fs::create_dir_all(&paths.config)?;
        let chain_path = paths.config.join("chain.json");
        let chain = dg_xch_core::consensus::chain_definition::ChainDefinition::falcon_development(
            "desktop-smoke-test".into(),
        );
        std::fs::write(&chain_path, serde_json::to_vec(&chain)?)?;
        settings.custom_chain = true;
        settings.chain_definition_path = chain_path.display().to_string();
        settings.genesis_header_hash = "00".repeat(32);
        settings.normalize_network()?;
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_profiles_select_the_requested_consensus() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: root.path().join("config"),
            data: root.path().join("data"),
        };
        let bls = smoke_settings(&paths, "bls").unwrap();
        assert!(bls.constants().unwrap().signature_profile.is_bls());
        assert_eq!(bls.constants().unwrap().bech32_prefix, "xch");
        assert!(smoke_settings(&paths, "typo").is_err());
        if cfg!(feature = "falcon") {
            let falcon = smoke_settings(&paths, "falcon").unwrap();
            falcon.validate().unwrap();
            assert!(!falcon.constants().unwrap().signature_profile.is_bls());
            assert_eq!(falcon.constants().unwrap().bech32_prefix, "dgx");
        } else {
            assert!(smoke_settings(&paths, "falcon").is_err());
        }
    }
}
