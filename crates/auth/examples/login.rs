//! Signs in with a Microsoft account and caches the tokens.
//!
//!     cargo run -p rapidbot-auth --example login -- accounts/main.json

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cache = std::env::args().nth(1).unwrap_or_else(|| "accounts/main.json".into());

    let mut auth = rapidbot_auth::Authenticator::new(&cache)?;
    let account = auth
        .login(|code| {
            println!("Sign in at {} with code {}", code.verification_uri, code.user_code);
        })
        .await?;
    println!("Logged in as {} ({})", account.name, account.id);
    println!("Chat signing keys: {}", if account.chat_keys.is_some() { "yes" } else { "no" });
    println!("Tokens cached in {cache}");
    Ok(())
}
