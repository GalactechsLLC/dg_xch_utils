use tokio::fs;

pub async fn newer(input_path: &str, output_path: &str) -> Result<bool, String> {
    if !std::path::Path::new(output_path).exists() {
        return Ok(true);
    }
    let input_md = fs::metadata(input_path).await
        .map_err(|_| "source does not exist".to_string())?;
    let output_md =fs::metadata(output_path).await
        .map_err(|_| "could not stat dest".to_string())?;
    Ok(input_md.modified()? >= output_md.modified()?)
}