//! `lz upgrade` — replace the running binary with a GitHub release asset
//! named `lz-<target>[.gz]` (e.g. `lz-aarch64-apple-darwin.gz`).

use std::io::Read;

use crate::cli::VERSION;

fn target() -> String {
    let arch = std::env::consts::ARCH;
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        "windows" => "pc-windows-msvc",
        other => other,
    };
    format!("{arch}-{os}")
}

pub async fn run(version: Option<String>, repo: String, check: bool) -> anyhow::Result<i32> {
    let client = reqwest::Client::builder()
        .user_agent(format!("lunarzero/{VERSION}"))
        .build()?;
    let url = match &version {
        Some(v) => format!("https://api.github.com/repos/{repo}/releases/tags/{v}"),
        None => format!("https://api.github.com/repos/{repo}/releases/latest"),
    };
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("could not fetch release info from {url}: HTTP {}", resp.status());
    }
    let release: serde_json::Value = resp.json().await?;
    let tag = release["tag_name"].as_str().unwrap_or("").to_string();
    let latest = tag.trim_start_matches('v');
    println!("current: v{VERSION}\nlatest:  {tag}");
    if latest == VERSION && version.is_none() {
        println!("already up to date");
        return Ok(0);
    }
    if check {
        return Ok(0);
    }
    let target = target();
    let assets = release["assets"].as_array().cloned().unwrap_or_default();
    let asset = assets
        .iter()
        .find(|a| {
            let n = a["name"].as_str().unwrap_or("");
            n == format!("lz-{target}") || n == format!("lz-{target}.gz")
        })
        .ok_or_else(|| anyhow::anyhow!("no asset for {target} in release {tag}"))?;
    let name = asset["name"].as_str().unwrap_or("").to_string();
    let dl = asset["browser_download_url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("asset has no download url"))?;
    println!("downloading {name}…");
    let bytes = client
        .get(dl)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec();
    let bytes = if name.ends_with(".gz") {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&bytes[..]).read_to_end(&mut out)?;
        out
    } else {
        bytes
    };
    let exe = std::env::current_exe()?;
    let tmp = exe.with_extension("new");
    std::fs::write(&tmp, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &exe)?;
    println!("installed {tag} to {}", exe.display());
    Ok(0)
}
