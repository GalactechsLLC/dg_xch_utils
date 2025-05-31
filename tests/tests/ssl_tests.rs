// #[tokio::test]
// pub async fn test_farmer_ws_client() -> Result<(), std::io::Error> {
//     use dg_xch_core::ssl::create_all_ssl;
//     use log::Level;
//     use dg_logger::DruidGardenLogger;
//     let _logger = DruidGardenLogger::build()
//     .use_colors(true)
//     .current_level(Level::Info)
//     .init()
//     .map_err(|e| Error::other( format!("{e:?}")))?;
//     let ssl_path = "/home/luna/ssl_test/";
//     create_all_ssl(ssl_path.as_ref(), true).unwrap();
//     Ok(())
// }

// #[test]
// pub fn test_ssl() {
//     use log::Level;
//     use dg_logger::DruidGardenLogger;
//     let _logger = DruidGardenLogger::build()
//     .use_colors(true)
//     .current_level(Level::Info)
//     .init()
//     .map_err(|e| Error::other( format!("{e:?}")))?;
//     let path = Path::new("/home/luna/ssl_test/");
//     create_all_ssl(path, false).unwrap();
//     if validate_all_ssl(path) {
//         info!("Validated SSL");
//     } else {
//         info!("Failed to Validated SSL");
//     }
// }
