//! Shared, self-hosted Druid Garden styles and fonts for HTTP dashboards.
use portfu::prelude::{Response, http::HeaderValue};

pub fn asset(path: &str) -> Option<Response> {
    let (bytes, content_type): (&'static [u8], &'static str) = match path {
        "/logo.jpg" => (include_bytes!("../static/druid/logo.jpg"), "image/jpeg"),
        "/site.css" => (
            include_bytes!("../static/druid/site.css"),
            "text/css; charset=utf-8",
        ),
        "/theme.js" => (
            include_bytes!("../static/druid/theme.js"),
            "text/javascript; charset=utf-8",
        ),
        "/sora-0.ttf" => (include_bytes!("../static/druid/sora-0.ttf"), "font/ttf"),
        "/sora-1.ttf" => (include_bytes!("../static/druid/sora-1.ttf"), "font/ttf"),
        "/sora-2.ttf" => (include_bytes!("../static/druid/sora-2.ttf"), "font/ttf"),
        "/sora-3.ttf" => (include_bytes!("../static/druid/sora-3.ttf"), "font/ttf"),
        "/manrope-0.ttf" => (include_bytes!("../static/druid/manrope-0.ttf"), "font/ttf"),
        "/manrope-1.ttf" => (include_bytes!("../static/druid/manrope-1.ttf"), "font/ttf"),
        "/manrope-2.ttf" => (include_bytes!("../static/druid/manrope-2.ttf"), "font/ttf"),
        "/manrope-3.ttf" => (include_bytes!("../static/druid/manrope-3.ttf"), "font/ttf"),
        "/playwrite-0.ttf" => (
            include_bytes!("../static/druid/playwrite-0.ttf"),
            "font/ttf",
        ),
        _ => return None,
    };
    let mut response = Response::ok(bytes).content_type(content_type);
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static("public, max-age=3600"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    Some(response)
}
