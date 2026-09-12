//! `lz auth` — store/list/remove provider API keys.

use lz_schema::api::AuthInfo;

use crate::cli::AuthCommand;

pub async fn run(cmd: AuthCommand) -> anyhow::Result<i32> {
    let paths = lz_core::Paths::detect();
    paths.ensure()?;
    let store = lz_core::provider::auth::AuthStore::new(&paths);
    match cmd {
        AuthCommand::List => {
            let all = store.all();
            if all.is_empty() {
                println!("no stored credentials ({})", store.path().display());
                return Ok(0);
            }
            for (provider, info) in all {
                let kind = match info {
                    AuthInfo::Api { .. } => "api key",
                    AuthInfo::OAuth { .. } => "oauth",
                };
                let at = store.location(&provider).unwrap_or("file");
                println!("{provider:<20} {kind}  ({at})");
            }
            Ok(0)
        }
        AuthCommand::Login {
            provider,
            key,
            keychain,
        } => {
            let catalog = lz_core::provider::catalog::embedded();
            let provider = match provider {
                Some(p) => p,
                None => {
                    let free = lz_core::provider::pool::catalog();
                    let mut ids: Vec<String> = catalog
                        .values()
                        .filter(|p| {
                            free.providers.contains_key(&p.id)
                                || p.npm
                                    .as_deref()
                                    .is_some_and(|n| lz_core::provider::protocol_for(n).is_some())
                        })
                        .map(|p| format!("{} ({})", p.id, p.name))
                        .collect();
                    for (id, fp) in &free.providers {
                        if !catalog.contains_key(id) {
                            ids.push(format!("{id} ({})", fp.name));
                        }
                    }
                    ids.sort();
                    ids.dedup();
                    let pick = inquire::Select::new("Provider", ids)
                        .with_page_size(15)
                        .prompt()?;
                    pick.split(' ').next().unwrap_or("").to_string()
                }
            };
            let key = match key {
                Some(k) => k,
                None => inquire::Password::new("API key")
                    .without_confirmation()
                    .with_display_mode(inquire::PasswordDisplayMode::Masked)
                    .prompt()?,
            };
            let want_keychain = keychain || keychain_default();
            if want_keychain {
                if !lz_core::provider::auth::keychain_available() {
                    anyhow::bail!("the OS keychain is not available here; omit --keychain to use auth.json");
                }
                store.set_in_keychain(&provider, &key)?;
                println!("stored credential for {provider} in the OS keychain");
            } else {
                store.set(&provider, AuthInfo::Api { key, metadata: None })?;
                println!("stored credential for {provider} in {}", store.path().display());
            }
            Ok(0)
        }
        AuthCommand::Migrate => {
            if !lz_core::provider::auth::keychain_available() {
                anyhow::bail!("the OS keychain is not available here");
            }
            let moved = store.migrate_to_keychain()?;
            if moved.is_empty() {
                println!("nothing to move — no file-stored keys");
            } else {
                println!("moved to the OS keychain: {}", moved.join(", "));
            }
            Ok(0)
        }
        AuthCommand::Logout { provider } => {
            let provider = match provider {
                Some(p) => p,
                None => {
                    let ids: Vec<String> = store.all().into_keys().collect();
                    if ids.is_empty() {
                        println!("no stored credentials");
                        return Ok(0);
                    }
                    inquire::Select::new("Remove credential for", ids).prompt()?
                }
            };
            store.remove(&provider)?;
            println!("removed {provider}");
            Ok(0)
        }
    }
}

/// `"auth": {"keychain": true}` in the global config makes login default to the keychain.
fn keychain_default() -> bool {
    let paths = lz_core::paths::Paths::detect();
    let dir = std::env::current_dir().unwrap_or_default();
    let worktree = lz_core::project::resolve(&dir).worktree;
    lz_core::config::load(lz_core::config::LoadInput {
        paths: &paths,
        directory: &dir,
        worktree: &worktree,
    })
    .ok()
    .and_then(|l| {
        l.raw
            .get("auth")
            .and_then(|a| a.get("keychain"))
            .and_then(|v| v.as_bool())
    })
    .unwrap_or(false)
}
