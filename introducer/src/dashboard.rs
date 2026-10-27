use portfu::prelude::{Response, http::HeaderValue};

pub fn response(body: impl AsRef<[u8]>, content_type: &'static str) -> Response {
    let mut response = Response::ok(body).content_type(content_type);
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert("content-security-policy", HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; font-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"));
    response
}
pub fn asset(path: &str) -> Option<Response> {
    if let Some(response) = dg_xch_servers::web_theme::asset(path) {
        return Some(response);
    }
    match path {
        "/" => Some(response(
            include_bytes!("../static/index.html"),
            "text/html; charset=utf-8",
        )),
        "/dashboard.css" => Some(response(
            include_bytes!("../static/dashboard.css"),
            "text/css; charset=utf-8",
        )),
        "/dashboard.js" => Some(response(
            include_bytes!("../static/dashboard.js"),
            "text/javascript; charset=utf-8",
        )),
        _ => None,
    }
}
