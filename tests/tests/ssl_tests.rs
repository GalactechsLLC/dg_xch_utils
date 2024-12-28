// #[tokio::test]
// pub async fn test_farmer_ws_client() -> Result<(), std::io::Error> {
//     use dg_xch_core::ssl::create_all_ssl;
//     use simple_logger::SimpleLogger;
//     SimpleLogger::new().env().init().unwrap_or_default();
//     let ssl_path = "/home/luna/ssl_test/";
//     create_all_ssl(ssl_path.as_ref(), true).unwrap();
//     Ok(())
// }

// #[test]
// pub fn test_ssl() {
//     use simple_logger::SimpleLogger;
//     SimpleLogger::new().init().unwrap();
//     let path = Path::new("/home/luna/ssl_test/");
//     create_all_ssl(path, false).unwrap();
//     if validate_all_ssl(path) {
//         info!("Validated SSL");
//     } else {
//         info!("Failed to Validated SSL");
//     }
// }
