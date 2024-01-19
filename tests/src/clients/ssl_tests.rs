#[tokio::test]
pub async fn test_farmer_ws_client() -> Result<(), std::io::Error> {
    use dg_xch_core::ssl::create_all_ssl;
    use simple_logger::SimpleLogger;
    SimpleLogger::new().env().init().unwrap_or_default();
    let ssl_path = "/home/luna/ssl_test/";
    create_all_ssl(ssl_path.as_ref(), true).unwrap();
    Ok(())
}
