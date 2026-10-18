use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RDM_LOG").unwrap_or_else(|_| "info".into()))
        .init();
    let path = rdm_daemon::socket_path();
    eprintln!("rdm-daemon listening at {}", path.display());
    rdm_daemon::run(path).await
}
