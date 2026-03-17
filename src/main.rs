#[tokio::main]
async fn main() {
    if let Err(error) = metastack_cli::run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
