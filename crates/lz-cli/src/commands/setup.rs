//! `lz setup` — first-run onboarding: pick a provider, get a key, verify it.

use lz_schema::api::AuthInfo;

use crate::commands::config::resolve_dir;

pub async fn exec() -> anyhow::Result<i32> {
    let directory = resolve_dir(&None)?;
    let engine = lz_core::Engine::start(lz_core::EngineOptions {
        directory,
        auto_approve: false,
        offline: false,
    })
    .await?;
    println!("LunarZero setup\n");
    let registry = engine.registry();
    let connected: Vec<String> = registry
        .connected()
        .filter(|p| p.id != lz_core::provider::pool::PROVIDER)
        .map(|p| p.id.clone())
        .collect();
    if !connected.is_empty() {
        println!("Already connected: {}\n", connected.join(", "));
    }
    let cat = lz_core::provider::pool::catalog();
    let mut choices: Vec<String> = cat
        .providers
        .iter()
        .filter(|(id, _)| !connected.contains(id))
        .map(|(id, p)| {
            format!(
                "{id:<24} free tier — {}",
                p.note.chars().take(60).collect::<String>()
            )
        })
        .collect();
    choices.push("openai                   paid (OPENAI_API_KEY)".into());
    choices.push(
        "anthropic-compatible…    any other OpenAI-compatible provider (configure in lunarzero.json)".into(),
    );
    choices.push("local                    I run Ollama / LM Studio on this machine".into());
    let pick = inquire::Select::new("Which provider do you want to connect first?", choices)
        .with_page_size(18)
        .prompt()?;
    let id = pick.split_whitespace().next().unwrap_or("").to_string();
    if id == "local" {
        println!(
            "\nStart Ollama (`ollama serve`, then `ollama pull qwen2.5-coder`) or LM Studio's server.\nLunarZero detects them on 127.0.0.1 automatically — run `lz` when the server is up."
        );
        engine.shutdown().await;
        return Ok(0);
    }
    if id.starts_with("anthropic-compatible") {
        println!(
            "\nAdd to lunarzero.json:\n{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"provider": {"myprovider": {"npm": "@ai-sdk/openai-compatible", "options": {"baseURL": "https://api.example.com/v1", "apiKey": "{env:MY_KEY}"}, "models": {"model-id": {"name": "Model"}}}}, "model": "myprovider/model-id"})
            )?
        );
        engine.shutdown().await;
        return Ok(0);
    }
    if let Some(p) = cat.providers.get(&id) {
        println!("\n{}: {}\nSignup / API keys: {}", p.name, p.note, p.signup);
        let open = inquire::Confirm::new("Open the signup page in your browser?")
            .with_default(true)
            .prompt()
            .unwrap_or(false);
        if open {
            lz_web::open_browser(&p.signup);
        }
    }
    let key = inquire::Password::new("Paste the API key:")
        .without_confirmation()
        .with_display_mode(inquire::PasswordDisplayMode::Masked)
        .prompt()?;
    let key = key.trim().to_string();
    if key.is_empty() {
        anyhow::bail!("no key entered");
    }
    engine.auth.set(&id, AuthInfo::Api { key, metadata: None })?;
    engine.reload().await?;
    // verify with the cheapest request we can make
    print!("Verifying… ");
    let registry = engine.registry();
    let Some(provider) = registry.providers.get(&id) else {
        anyhow::bail!("unknown provider {id}")
    };
    let model = registry
        .default_model(&engine.config())
        .filter(|m| m.provider_id == id)
        .cloned()
        .or_else(|| {
            provider
                .models
                .values()
                .find(|m| m.protocol.is_some() && lz_core::provider::is_chat_model(&m.id))
                .cloned()
        });
    let Some(model) = model else {
        println!("saved (no model listed for {id} yet; pick one with /models)");
        engine.shutdown().await;
        return Ok(0);
    };
    let (protocol, endpoint) = registry.endpoint(&model).map_err(|e| anyhow::anyhow!(e))?;
    let req = lz_core::llm::types::LlmRequest {
        model_id: model.api_id.clone(),
        messages: vec![lz_core::llm::types::LlmMessage::User {
            content: vec![lz_core::llm::types::ContentPart::Text {
                text: "Reply with the single word: ready".into(),
            }],
        }],
        generation: lz_core::llm::types::Generation {
            max_tokens: Some(8),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut rx = registry.client().stream(
        protocol,
        endpoint,
        req,
        tokio_util::sync::CancellationToken::new(),
    );
    let mut text = String::new();
    let mut err = None;
    while let Some(ev) = rx.recv().await {
        match ev {
            Ok(lz_core::llm::types::LlmEvent::TextDelta { text: t, .. }) => text.push_str(&t),
            Err(e) => {
                err = Some(e);
                break;
            }
            _ => {}
        }
    }
    match err {
        None => println!("ok — {} answered \"{}\"", model.full_id(), text.trim()),
        Some(e) => println!("the key was saved but a test request failed: {e}"),
    }
    let pool_connected = registry
        .providers
        .get(lz_core::provider::pool::PROVIDER)
        .is_some_and(|p| p.connected());
    println!(
        "\nDone. Run `lz` to start{}.",
        if pool_connected {
            " — the default model is lunar/auto (routes across your free keys)"
        } else {
            ""
        }
    );
    println!(
        "Add more free providers any time: lz setup · lz pool setup · /connect in the TUI · the web portal."
    );
    engine.shutdown().await;
    Ok(0)
}
