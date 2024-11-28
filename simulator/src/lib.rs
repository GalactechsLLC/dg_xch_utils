use log::info;
use portfu::prelude::ServerBuilder;
use simple_logger::SimpleLogger;
use std::env;

pub async fn start_simulator() -> Result<(), std::io::Error> {
    SimpleLogger::new().env().init().unwrap();
    let hostname = env::var("SIMULATOR_HOSTNAME").unwrap_or("0.0.0.0".to_string());
    let port = env::var("SIMULATOR_PORT")
        .map(|s| s.parse().unwrap())
        .unwrap_or(8080u16);
    let server = ServerBuilder::default().host(hostname).port(port).build();
    info!("Starting Server");
    server.run().await
}
