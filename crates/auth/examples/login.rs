//! Signs in with a Microsoft account and caches the tokens.
//!
//!     cargo run -p rapidbot-auth --example login -- main

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let alias = std::env::args().nth(1).unwrap_or_else(|| "main".into());
    let dir = std::env::var_os("RAPIDBOT_ACCOUNTS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "accounts".into());

    let store = rapidbot_auth::AccountStore::open(&dir)?;
    let mut auth = store.authenticator(&alias)?;
    if let Ok(client_id) = std::env::var("RAPIDBOT_CLIENT_ID") {
        auth = auth.with_client_id(client_id);
    }
    let account = auth
        .login(|code| {
            println!(
                "Sign in at {} with code {}",
                code.verification_uri, code.user_code
            );
        })
        .await?;
    store.remember(&alias, &account)?;
    println!("Logged in as {} ({})", account.name, account.id);
    println!(
        "Chat signing keys: {}",
        if account.chat_keys.is_some() {
            "yes"
        } else {
            "no"
        }
    );
    println!("Account {alias} saved in {}", store.root().display());
    Ok(())
}
