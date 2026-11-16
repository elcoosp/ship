//! Minimal GitHub REST client over a persistent connection (ureq 3).
//! PR creation: single POST /repos/{o}/{r}/pulls. On HTTP error we read the
//! body to surface GitHub's actual message, then the caller falls back to gh.

use crate::shell::{Shell, run_checked};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

pub struct Gh {
    agent: ureq::Agent,
    token: String,
    owner: String,
    name: String,
}

/// Parse owner/repo from a GitHub remote URL, including SSH host aliases
/// like `git@github.com-<account>:<owner>/<repo>.git` (multi-account setups).
fn parse_nwo(url: &str) -> Option<(String, String)> {
    let url = url.trim().trim_end_matches(".git");
    let idx = url.find("github.com")?;

    // Ensure "github.com" is not part of a larger word (e.g., "notgithub.com").
    // It must be at the start of the string or preceded by '@', '/', or '-'.
    if idx > 0 {
        let prev_char = url.chars().nth(idx - 1)?;
        if prev_char != '@' && prev_char != '/' && prev_char != '-' {
            return None;
        }
    }

    let after = &url[idx + "github.com".len()..];
    // Skip any host-alias suffix up to the host/path separator (':' scp, '/' url).
    let sep = after.find([':', '/'])?;
    let path = after[sep..].trim_start_matches([':', '/']);
    let mut parts = path.split('/');
    let owner = parts.next()?.to_string();
    let name = parts.next()?.to_string();
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner, name))
}

impl Gh {
    pub fn new(shell: &dyn Shell) -> Result<Self> {
        let token = std::env::var("GH_TOKEN")
            .or_else(|_| std::env::var("GITHUB_TOKEN"))
            .ok()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| {
                run_checked(shell, &["gh", "auth", "token"])
                    .ok()
                    .map(|o| o.stdout.trim().to_string())
                    .filter(|t| !t.is_empty())
            })
            .context("no GitHub token (gh auth token / GH_TOKEN)")?;
        let url = run_checked(shell, &["git", "remote", "get-url", "origin"])?
            .stdout
            .trim()
            .to_string();
        let (owner, name) = parse_nwo(&url)
            .with_context(|| format!("cannot parse owner/repo from remote url: {url}"))?;
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .timeout_connect(Some(Duration::from_secs(5)))
            .build();
        let agent: ureq::Agent = config.into();
        Ok(Self {
            agent,
            token,
            owner,
            name,
        })
    }

    pub fn create_pr(&self, branch: &str, base: &str, title: &str, body: &str) -> Result<String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls",
            self.owner, self.name
        );
        let mut resp = self
            .agent
            .post(url.as_str())
            .config()
            .http_status_as_error(false)
            .build()
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("User-Agent", "ship/0.2")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(json!({ "title": title, "head": branch, "base": base, "body": body }))
            .context("REST create PR request failed")?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().context("REST read body")?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status >= 400 {
            let msg = v
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or(text.trim());
            let errs = v.get("errors").map(|e| e.to_string()).unwrap_or_default();
            bail!(
                "REST create PR {status} on {}/{}: {msg} {errs}",
                self.owner,
                self.name
            );
        }
        let html = v["html_url"]
            .as_str()
            .context("create PR returned no html_url")?
            .to_string();
        Ok(html)
    }
}

#[cfg(test)]
mod tests {
    use super::parse_nwo;

    #[test]
    fn parses_ssh_alias_and_standard_urls() {
        let want = Some(("elcoosp".to_string(), "oxide".to_string()));
        assert_eq!(parse_nwo("git@github.com-elcoosp:elcoosp/oxide.git"), want);
        assert_eq!(parse_nwo("git@github.com:elcoosp/oxide.git"), want);
        assert_eq!(parse_nwo("https://github.com/elcoosp/oxide.git"), want);
        assert_eq!(parse_nwo("https://github.com/elcoosp/oxide"), want);
        assert_eq!(parse_nwo("ssh://git@github.com/elcoosp/oxide.git"), want);
        assert_eq!(
            parse_nwo("ssh://git@github.com-elcoosp/elcoosp/oxide.git"),
            want
        );
        // Reject false positives
        assert_eq!(parse_nwo("git@notgithub.com:foo/bar.git"), None);
        assert_eq!(parse_nwo("https://fakegithub.com/foo/bar"), None);
    }
}
