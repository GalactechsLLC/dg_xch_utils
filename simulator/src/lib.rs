use dg_logger::DruidGardenLogger;
use log::info;
use log::Level;
use portfu::prelude::ServerBuilder;
use std::env;
use std::io::Error;

pub async fn start_simulator() -> Result<(), Error> {
    let _logger = DruidGardenLogger::build()
        .use_colors(true)
        .current_level(Level::Info)
        .init()
        .map_err(|e| Error::other(format!("{e:?}")))?;
    let hostname = env::var("SIMULATOR_HOSTNAME").unwrap_or("0.0.0.0".to_string());
    let port = env::var("SIMULATOR_PORT")
        .map(|s| s.parse().unwrap())
        .unwrap_or(8080u16);
    let server = ServerBuilder::default().host(hostname).port(port).build();
    info!("Starting Server");
    server.run().await
}
