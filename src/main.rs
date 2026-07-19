#[tokio::main]
async fn main() -> anyhow::Result<()> {
    codex_sync::cli::run().await
}
