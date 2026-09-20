#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented
    )
)]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use dg_xch_gui::app::Desktop;
use dg_xch_gui::backend::Backend;
use dg_xch_gui::config::{AppPaths, Settings};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let smoke = match arguments.as_slice() {
        [] => false,
        [argument] if argument == "--smoke-test" => true,
        _ => return Err("usage: dg_xch_gui [--smoke-test]".into()),
    };
    let temporary = smoke.then(tempfile::tempdir).transpose()?;
    let paths = if let Some(directory) = &temporary {
        AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
        }
    } else {
        AppPaths::discover()?
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
            .with_title("Galactechs | Network desk")
            .with_inner_size([1280.0, 840.0])
            .with_min_inner_size([900.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Galactechs Network Desk",
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
