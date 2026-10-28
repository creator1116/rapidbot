//! `rapidbot-auth`: manage the accounts used by the bot.
//!
//!     rapidbot-auth login [alias]
//!     rapidbot-auth status [alias]
//!     rapidbot-auth logout <alias>
//!
//! The token cache is encrypted and its key is kept in the OS keyring. Set
//! `RAPIDBOT_ACCOUNTS` to choose the directory (default `accounts`).

use std::error::Error;
use std::path::PathBuf;

use rapidbot_auth::AccountStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    let dir = std::env::var_os("RAPIDBOT_ACCOUNTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("accounts"));
    let store = AccountStore::open(&dir)?;

    match command.as_str() {
        "login" => {
            let alias = args.next().unwrap_or_else(|| "main".into());
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
            println!("Logged in as {} ({}) as {alias}", account.name, account.id);
            println!(
                "Chat signing keys: {}",
                if account.chat_keys.is_some() {
                    "yes"
                } else {
                    "no"
                }
            );
            println!("Account directory: {}", dir.display());
        }
        "status" => {
            let aliases: Vec<String> = match args.next() {
                Some(alias) => vec![alias],
                None => store.list()?.into_iter().map(|a| a.alias).collect(),
            };
            if aliases.is_empty() {
                println!("No accounts in {}", dir.display());
                return Ok(());
            }
            let accounts = store.list()?;
            for alias in aliases {
                let Some(record) = accounts.iter().find(|a| a.alias == alias) else {
                    println!("{alias}: not found");
                    continue;
                };
                println!("{}: {} ({})", record.alias, record.name, record.id);
                match store.status(&alias) {
                    Ok(status) => {
                        println!(
                            "  tokens: {}",
                            status
                                .minecraft_expires_at
                                .map_or_else(|| "none".into(), |at| format!("expire at {at}")),
                        );
                        println!(
                            "  refresh token: {}; chat keys: {}",
                            if status.has_refresh_token {
                                "yes"
                            } else {
                                "no"
                            },
                            status
                                .chat_keys_expire_at
                                .map_or_else(|| "none".into(), |at| format!("good until {at}")),
                        );
                    }
                    Err(e) => println!("  cache unavailable: {e}"),
                }
            }
        }
        "logout" => {
            let Some(alias) = args.next() else {
                return Err("usage: rapidbot-auth logout <alias>".into());
            };
            match store.remove(&alias) {
                Ok(()) => println!("Logged out {alias}"),
                Err(e) => return Err(format!("could not log out {alias}: {e}").into()),
            }
        }
        "help" | "-h" | "--help" => {
            println!("usage: rapidbot-auth <login [alias] | status [alias] | logout <alias>>");
            println!("account directory: {}", dir.display());
        }
        other => return Err(format!("unknown command {other:?}; try `rapidbot-auth help`").into()),
    }
    Ok(())
}
