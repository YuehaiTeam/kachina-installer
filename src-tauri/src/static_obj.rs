lazy_static::lazy_static! {
    pub static ref REQUEST_CLIENT: reqwest::Client = reqwest::Client::builder()
        .user_agent(format!("KachinaInstaller/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(5))
        .gzip(true)
        .brotli(true)
        .zstd(true)
        .build()
        .unwrap();
}
