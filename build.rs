use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Router and headless Linux builds do not require Node or UI assets.
    if env::var_os("CARGO_FEATURE_LINUX_UI").is_none() {
        return;
    }

    let dist = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("linux-web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());

    for required in ["index.html", "shell.js", "style.css"] {
        assert!(
            dist.join(required).is_file(),
            "Missing Linux UI asset {required}; run npm --prefix linux-web run build first"
        );
    }

    // Generate the asset registry from the current Vite build rather than
    // maintaining a parallel list of Rollup chunk names in Rust.
    let mut files: Vec<_> = fs::read_dir(&dist)
        .expect("read Linux UI dist directory")
        .collect::<Result<_, _>>()
        .expect("list Linux UI assets");
    files.sort_by_key(|entry| entry.file_name());

    let mut source = String::from(
        "fn embedded_asset(name: &str) -> Option<(&'static str, &'static [u8])> {\n    match name {\n",
    );
    for entry in files {
        if !entry.file_type().expect("asset file type").is_file() {
            continue;
        }
        let path = entry.path();
        let content_type = match path.extension().and_then(|ext| ext.to_str()) {
            Some("js") => "text/javascript",
            Some("css") => "text/css",
            _ => continue,
        };
        let name = entry.file_name();
        let name = name.to_str().expect("UTF-8 asset name");
        let path = path.to_str().expect("UTF-8 asset path");
        println!("cargo:rerun-if-changed={path}");
        source.push_str(&format!(
            "        {name:?} => Some(({content_type:?}, include_bytes!({path:?}))),\n"
        ));
    }
    source.push_str("        _ => None,\n    }\n}\n");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"))
        .join("linux_ui_assets.rs");
    fs::write(output, source).expect("write Linux UI asset registry");
}
