use std::time::Duration;

lazy_static::lazy_static! {
    pub static ref REQUEST_CLIENT: reqwest::Client = reqwest::Client::builder()
        .user_agent(format!("KachinaInstaller/{}", env!("CARGO_PKG_VERSION")))
        .gzip(true)
        .zstd(true)
        .read_timeout(Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
}
