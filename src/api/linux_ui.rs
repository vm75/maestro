use axum::{
    extract::Path,
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};

include!(concat!(env!("OUT_DIR"), "/linux_ui_assets.rs"));

pub(super) fn index() -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Html(include_str!("../../linux-web/dist/index.html")),
    )
        .into_response()
}

pub(super) async fn asset(Path(name): Path<String>) -> Response {
    let Some((content_type, content)) = embedded_asset(&name) else {
        return StatusCode::NOT_FOUND.into_response();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeds_current_vite_assets() {
        assert_eq!(embedded_asset("shell.js").unwrap().0, "text/javascript");
        assert_eq!(embedded_asset("style.css").unwrap().0, "text/css");
        let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("linux-web/dist");
        for entry in std::fs::read_dir(dist).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_file()
                && matches!(path.extension().and_then(|ext| ext.to_str()), Some("js" | "css"))
            {
                let name = entry.file_name();
                let (_, embedded) = embedded_asset(name.to_str().unwrap()).unwrap();
                assert_eq!(embedded, std::fs::read(path).unwrap().as_slice());
            }
        }
        assert!(embedded_asset("../Cargo.toml").is_none());
        assert!(embedded_asset("unknown.js").is_none());
    }
}
