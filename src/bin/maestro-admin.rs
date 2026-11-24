// A separate Linux-only, root-owned one-shot executable. Never a network service.
#[tokio::main]
async fn main() {
    if let Err(error) = maestro::platforms::admin::helper_main().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
