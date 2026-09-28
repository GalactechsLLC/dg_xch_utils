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
        Settings::default()
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
