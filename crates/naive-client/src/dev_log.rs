//! `naive submit-log` — submit dev.log as a GitHub issue for engine feedback.

use std::path::Path;

use crate::project_config::NaiveConfig;

pub fn submit_dev_log(config: &NaiveConfig, project_root: &Path) -> Result<(), String> {
    let dev_log_path = project_root.join("dev.log");

    if !dev_log_path.exists() {
        return Err(
            "No dev.log found in project root.\n  \
             Run `naive init <name>` to create a project with a dev.log template,\n  \
             or create one manually."
                .to_string(),
        );
    }

    let contents =
        std::fs::read_to_string(&dev_log_path).map_err(|e| format!("Failed to read dev.log: {}", e))?;

    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Err("dev.log is empty. Fill it out before submitting.".to_string());
    }

    // Check for GitHub token
    let token = match std::env::var("NAIVE_GITHUB_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => {
            return Err(
                "NAIVE_GITHUB_TOKEN environment variable not set.\n\n\
                 To submit engine feedback:\n\
                 1. Create a GitHub Personal Access Token at https://github.com/settings/tokens\n\
                    (needs 'repo' or 'public_repo' scope)\n\
                 2. Export it:\n\
                    export NAIVE_GITHUB_TOKEN=ghp_your_token_here\n\
                 3. Run `naive submit-log` again"
                    .to_string(),
            );
        }
    };

    let engine_version = env!("CARGO_PKG_VERSION");
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();

    let title = format!("[Dev Log] {} — v{}", config.name, engine_version);
    let body = format!(
        "**Project:** {} v{}  \n\
         **Engine:** nAIVE v{}  \n\
         **Submitted:** {}  \n\n\
         ---\n\n\
         {}",
        config.name, config.version, engine_version, now, trimmed
    );

    let payload = serde_json::json!({
        "title": title,
        "body": body,
        "labels": ["dev-log"]
    });

    println!("Submitting dev.log as GitHub issue...");

    let response = ureq::post("https://api.github.com/repos/poro/nAIVE/issues")
        .set("Authorization", &format!("Bearer {}", token))
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", &format!("nAIVE-engine/{}", engine_version))
        .set("X-GitHub-Api-Version", "2022-11-28")
        .send_json(&payload)
        .map_err(|e| format!("GitHub API request failed: {}", e))?;

    let response_body: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse GitHub response: {}", e))?;

    let issue_url = response_body["html_url"]
        .as_str()
        .unwrap_or("(URL not found in response)");

    println!("  Issue created: {}", issue_url);

    // Append submission record to dev.log
    let appendix = format!("\n\n## Submitted: {} — {}\n", now, issue_url);
    let mut updated = contents.clone();
    updated.push_str(&appendix);
    std::fs::write(&dev_log_path, updated)
        .map_err(|e| format!("Failed to update dev.log: {}", e))?;

    println!("  dev.log updated with submission record.");

    Ok(())
}
