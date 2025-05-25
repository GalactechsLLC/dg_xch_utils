use crate::app::DgXchGui;
use crate::config::Config;
use dg_logger::DruidGardenLogger;
use log::Level;
use std::env;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod app;
mod components;
mod config;
mod scenes;
mod state;

#[tokio::main]
async fn main() -> eframe::Result {
    let _logger = DruidGardenLogger::build()
        .use_colors(true)
        .current_level(Level::Info)
        .init()
        .map_err(|e| {
            eframe::Error::AppCreation(format!("Could not initialise logging: {e:?}").into())
        })?;
    let options = eframe::NativeOptions::default();
    let config_path_str = env::var("DG_CONFIG").unwrap_or_else(|_| "config.yaml".into());
    let config_path = Path::new(&config_path_str);
    let config = Config::try_from(config_path).unwrap_or_default();
    config.save_as_yaml(config_path).unwrap_or_default();
    let shutdown_signal = Arc::new(AtomicBool::new(true));
    let result = eframe::run_native(
        "Druid Garden GUI",
        options,
        Box::new(|cc| {
            Ok(Box::new(
                DgXchGui::new(cc, config, shutdown_signal.clone()).unwrap(),
            ))
        }),
    );
    shutdown_signal.store(false, Ordering::SeqCst);
    result
}
