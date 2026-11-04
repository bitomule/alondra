#[tokio::main]
async fn main() -> anyhow::Result<()> {
    alondra_relay::run().await
}
