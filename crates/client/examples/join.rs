//! Joins a server and idles like a player who walked away from the keyboard.
//!
//!     cargo run --release -p rapidbot-client --example join -- localhost:25565 Steve
//!     cargo run --release -p rapidbot-client --example join -- play.example.net --microsoft main

use rapidbot_client::{Account, ClientConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let address = args.first().cloned().unwrap_or_else(|| "localhost".into());
    let account = match args.get(1).map(String::as_str) {
        Some("--microsoft") => {
            let alias = args.get(2).cloned().unwrap_or_else(|| "main".into());
            let dir = std::env::var_os("RAPIDBOT_ACCOUNTS")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| "accounts".into());
            let store = rapidbot_auth::AccountStore::open(dir)?;
            let mut auth = store.authenticator(&alias)?;
            if let Ok(client_id) = std::env::var("RAPIDBOT_CLIENT_ID") {
                auth = auth.with_client_id(client_id);
            }
            let account = auth
                .login(|code| {
                    println!(
                        "Sign in at {} with code {}",
                        code.verification_uri, code.user_code
                    )
                })
                .await?;
            store.remember(&alias, &account)?;
            Account::Microsoft(account)
        }
        Some(name) => Account::Offline { name: name.into() },
        None => Account::Offline {
            name: "rapidbot".into(),
        },
    };

    let mut config = ClientConfig::new(address, account);
    if let Ok(proxy) = std::env::var("RAPIDBOT_PROXY") {
        config.proxy = Some(proxy.parse()?);
    }
    let reason = rapidbot_client::run(config).await;
    tracing::warn!("connection ended: {reason}");
    Ok(())
}
