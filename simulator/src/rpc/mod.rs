use dg_logger::DruidGardenLogger;
use log::Level;
use log::info;
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
    let port = match env::var("SIMULATOR_PORT") {
        Ok(value) => value.parse::<u16>().map_err(|error| {
            Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid SIMULATOR_PORT: {error}"),
            )
        })?,
        Err(env::VarError::NotPresent) => 8080,
        Err(error) => return Err(Error::new(std::io::ErrorKind::InvalidInput, error)),
    };
    let server = ServerBuilder::default().host(hostname).port(port).build();
    info!("Starting Server");
    server.run().await.map_err(|e| Error::other(e.to_string()))
}
