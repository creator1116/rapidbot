//! `rapidbot-auth`: manage the accounts used by the bot.
//!
//!     rapidbot-auth login [alias]
//!     rapidbot-auth refresh [--all] [alias]
//!     rapidbot-auth list
//!     rapidbot-auth status [--json] [alias]
//!     rapidbot-auth logout <alias>
//!
//! The token cache is encrypted and its key is kept in the OS keyring. Set
//! `RAPIDBOT_ACCOUNTS` to choose the directory (default `accounts`).

use std::error::Error;
use std::path::PathBuf;

use rapidbot_auth::AccountStore;
use serde_json::json;

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
        "refresh" => {
            let (flags, values) = split_args(args, &["--all"])?;
            let all = flags.iter().any(|flag| flag == "--all");
            if values.len() > 1 || (all && !values.is_empty()) {
                return Err("usage: rapidbot-auth refresh [--all] [alias]".into());
            }
            let aliases: Vec<String> = if all {
                store.list()?.into_iter().map(|a| a.alias).collect()
            } else {
                vec![values.into_iter().next().unwrap_or_else(|| "main".into())]
            };
            if aliases.is_empty() {
                println!("No accounts in {}", dir.display());
                return Ok(());
            }
            let mut failed = 0;
            for alias in aliases {
                let mut auth = match store.authenticator(&alias) {
                    Ok(auth) => auth,
                    Err(e) => {
                        eprintln!("could not open {alias}: {e}");
                        failed += 1;
                        continue;
                    }
                };
                if let Ok(client_id) = std::env::var("RAPIDBOT_CLIENT_ID") {
                    auth = auth.with_client_id(client_id);
                }
                match auth.refresh().await {
                    Ok(account) => {
                        store.remember(&alias, &account)?;
                        println!("Refreshed {} ({}) as {alias}", account.name, account.id);
                    }
                    Err(e) => {
                        eprintln!("could not refresh {alias}: {e}");
                        failed += 1;
                    }
                }
            }
            if failed > 0 {
                return Err(format!("{failed} account(s) could not be refreshed").into());
            }
        }
        "list" => {
            if args.next().is_some() {
                return Err("usage: rapidbot-auth list".into());
            }
            let accounts = store.list()?;
            if accounts.is_empty() {
                println!("No accounts in {}", dir.display());
                return Ok(());
            }
            for account in accounts {
                println!("{}: {} ({})", account.alias, account.name, account.id);
            }
        }
        "status" => {
            let (flags, values) = split_args(args, &["--json"])?;
            let json_output = flags.iter().any(|flag| flag == "--json");
            if values.len() > 1 {
                return Err("usage: rapidbot-auth status [--json] [alias]".into());
            }
            let accounts = store.list()?;
            let aliases: Vec<String> = match values.into_iter().next() {
                Some(alias) => vec![alias],
                None => accounts.iter().map(|a| a.alias.clone()).collect(),
            };
            if aliases.is_empty() {
                println!("No accounts in {}", dir.display());
                return Ok(());
            }

            if json_output {
                let mut output = Vec::new();
                for alias in aliases {
                    let record = accounts.iter().find(|a| a.alias == alias);
                    let status = store.status(&alias);
                    let (status, error) = match status {
                        Ok(status) => (Some(status), None),
                        Err(e) => (None, Some(e.to_string())),
                    };
                    output.push(match record {
                        Some(record) => json!({
                            "alias": record.alias,
                            "name": record.name,
                            "id": record.id,
                            "last_login": record.last_login,
                            "status": status,
                            "error": error,
                        }),
                        None => json!({
                            "alias": alias,
                            "status": status,
                            "error": error.or(Some("not found".into())),
                        }),
                    });
                }
                println!("{}", serde_json::to_string_pretty(&output)?);
                return Ok(());
            }

            for alias in aliases {
                let Some(record) = accounts.iter().find(|a| a.alias == alias) else {
                    println!("{alias}: not found");
                    continue;
                };
                println!("{}: {} ({})", record.alias, record.name, record.id);
                match store.status(&alias) {
                    Ok(status) => {
                        println!(
                            "  Microsoft token: {}",
                            status
                                .msa_expires_at
                                .map_or_else(|| "none".into(), |at| format!("expires at {at}")),
                        );
                        println!(
                            "  Minecraft token: {}",
                            status
                                .minecraft_expires_at
                                .map_or_else(|| "none".into(), |at| format!("expires at {at}")),
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
            println!(
                "usage: rapidbot-auth <login [alias] | refresh [--all] [alias] | list | status [--json] [alias] | logout <alias>>"
            );
            println!("account directory: {}", dir.display());
        }
        other => return Err(format!("unknown command {other:?}; try `rapidbot-auth help`").into()),
    }
    Ok(())
}

fn split_args(
    args: impl Iterator<Item = String>,
    allowed_flags: &[&str],
) -> Result<(Vec<String>, Vec<String>), Box<dyn Error>> {
    let mut flags = Vec::new();
    let mut values = Vec::new();
    for arg in args {
        if arg.starts_with('-') {
            if !allowed_flags.contains(&arg.as_str()) {
                return Err(format!("unknown option {arg:?}").into());
            }
            flags.push(arg);
        } else {
            values.push(arg);
        }
    }
    Ok((flags, values))
}
