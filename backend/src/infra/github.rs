use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
struct CreateRepoPayload {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    private: bool,
    auto_init: bool,
}

#[derive(Debug, Deserialize)]
pub struct GitHubRepo {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    pub clone_url: String,
    pub html_url: String,
    pub private: bool,
}

pub async fn create_repo(
    token: &str,
    name: &str,
    description: Option<&str>,
    private: bool,
    org: Option<&str>,
) -> anyhow::Result<GitHubRepo> {
    let client = Client::new();
    let url = match org {
        Some(o) => format!("https://api.github.com/orgs/{}/repos", o),
        None => "https://api.github.com/user/repos".to_string(),
    };

    let payload = CreateRepoPayload {
        name: name.to_string(),
        description: description.map(|s| s.to_string()),
        private,
        auto_init: true,
    };

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "dealtech-code-agent/1.0")
        .json(&payload)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("GitHub API {} — {}", status, text);
    }

    Ok(resp.json::<GitHubRepo>().await?)
}
