use std::io::{Error, ErrorKind};

pub async fn bounded_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(Error::other)? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "HTTP response exceeds byte limit",
            ));
        }
        body.try_reserve(chunk.len()).map_err(Error::other)?;
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
