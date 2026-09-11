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
                println!("{provider:<20} {kind}");
            }
            Ok(0)
        }
        AuthCommand::Login { provider, key } => {
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
            store.set(&provider, AuthInfo::Api { key, metadata: None })?;
            println!("stored credential for {provider} in {}", store.path().display());
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
