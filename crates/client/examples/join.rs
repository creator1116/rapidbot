//! Joins a server and idles like a player who walked away from the keyboard.
//!
//!     cargo run --release -p rapidbot-client --example join -- localhost:25565 Steve
//!     cargo run --release -p rapidbot-client --example join -- play.example.net --microsoft accounts/main.json

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
            let cache = args.get(2).cloned().unwrap_or_else(|| "accounts/main.json".into());
            let mut auth = rapidbot_auth::Authenticator::new(&cache)?;
            let account = auth
                .login(|code| println!("Sign in at {} with code {}", code.verification_uri, code.user_code))
                .await?;
            Account::Microsoft(account)
        }
        Some(name) => Account::Offline { name: name.into() },
        None => Account::Offline { name: "rapidbot".into() },
    };

    let reason = rapidbot_client::run(ClientConfig::new(address, account)).await;
    tracing::warn!("connection ended: {reason}");
    Ok(())
}
