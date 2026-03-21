#[tokio::main]
async fn main() {
    std::process::exit(metastack_cli::run().await);
}
