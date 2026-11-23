use axum::{
    extract::Path,
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};

pub(super) fn index() -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Html(include_str!("../../linux-web/dist/index.html")),
    )
        .into_response()
}

pub(super) async fn asset(Path(name): Path<String>) -> Response {
    let (content_type, content): (&str, &[u8]) = match name.as_str() {
        "shell.js" => (
            "text/javascript",
            include_bytes!("../../linux-web/dist/shell.js"),
        ),
        "Overview.js" => (
            "text/javascript",
            include_bytes!("../../linux-web/dist/Overview.js"),
        ),
        "Applications.js" => (
            "text/javascript",
            include_bytes!("../../linux-web/dist/Applications.js"),
        ),
        "Services.js" => (
            "text/javascript",
            include_bytes!("../../linux-web/dist/Services.js"),
        ),
        "Shares.js" => (
            "text/javascript",
            include_bytes!("../../linux-web/dist/Shares.js"),
        ),
        "Updates.js" => (
            "text/javascript",
            include_bytes!("../../linux-web/dist/Updates.js"),
        ),
        "select.js" => (
            "text/javascript",
            include_bytes!("../../linux-web/dist/select.js"),
        ),
        "style.css" => ("text/css", include_bytes!("../../linux-web/dist/style.css")),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        content,
    )
        .into_response()
}
