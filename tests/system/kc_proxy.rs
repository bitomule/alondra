//! Test-image-only fixed loopback adapter. No configurable upstream or production use.
#[tokio::main]
async fn main() -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18082").await?;
    std::fs::write("/tmp/system-proxy-ready", b"ready")?;
    loop {
        let (mut incoming, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Ok(mut upstream) = tokio::net::TcpStream::connect("keycloak:8080").await {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    tokio::io::copy_bidirectional(&mut incoming, &mut upstream),
                )
                .await;
            }
        });
    }
}
