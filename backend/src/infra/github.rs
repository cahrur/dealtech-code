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

#[derive(Debug, Deserialize)]
pub struct GitHubPR {
    pub number: u64,
    pub html_url: String,
    pub title: String,
}

/// Extract "owner/repo" from a GitHub URL (HTTPS or SSH).
pub fn extract_repo_path(repo_url: &str) -> Option<String> {
    // https://github.com/owner/repo or https://github.com/owner/repo.git
    if let Some(rest) = repo_url.strip_prefix("https://github.com/") {
        return Some(rest.trim_end_matches(".git").to_string());
    }
    // git@github.com:owner/repo.git
    if let Some(rest) = repo_url.strip_prefix("git@github.com:") {
        return Some(rest.trim_end_matches(".git").to_string());
    }
    None
}

/// Create a pull request on GitHub.
/// Returns the PR URL on success.
pub async fn create_pr(
    token: &str,
    repo_url: &str,
    branch: &str,
    base: &str,
    title: &str,
    body: &str,
) -> anyhow::Result<GitHubPR> {
    let repo_path = extract_repo_path(repo_url)
        .ok_or_else(|| anyhow::anyhow!("Cannot extract owner/repo from URL: {}", repo_url))?;

    static GH_CLIENT: std::sync::OnceLock<Client> = std::sync::OnceLock::new();
    let client = GH_CLIENT.get_or_init(Client::new);

    let url = format!("https://api.github.com/repos/{}/pulls", repo_path);
    let payload = serde_json::json!({
        "title": title,
        "body": body,
        "head": branch,
        "base": base,
    });

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
        // 422 = PR already exists or branch already has open PR
        if status.as_u16() == 422 {
            anyhow::bail!("PR sudah ada atau branch tidak ada commit baru");
        }
        anyhow::bail!("GitHub API {} — {}", status, text);
    }

    Ok(resp.json::<GitHubPR>().await?)
}
