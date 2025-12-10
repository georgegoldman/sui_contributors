use axum::{
    extract::Query, http::StatusCode, response::Json, routing::get, Extension, Router,
};
use dotenv::dotenv;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tokio::net::TcpListener;

// ------------------- Structs -------------------

#[derive(Debug, Deserialize)]
struct SearchParams {
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct CodeSearchResult {
    total_count: Option<u32>, // GitHub sometimes hides this (null)
}

#[derive(Debug, Deserialize)]
struct DeveloperQuery {
    username: String,
}

#[derive(Debug, Deserialize)]
struct GitHubCodeSearchResponse {
    items: Vec<CodeResult>,
}

#[derive(Debug, Deserialize)]
struct CodeResult {
    repository: Repository,
}

#[derive(Debug, Deserialize, Clone)]
struct Repository {
    full_name: String,
    html_url: String,
}

#[derive(Debug, Deserialize, Clone)]
struct Contributor {
    login: String,
    avatar_url: String,
    html_url: String,
    contributions: u32,
}

#[derive(Debug, Serialize)]
struct UserResponse {
    login: String,
    avatar_url: String,
    profile_url: String,
    total_contributions: u32,
    repositories: Vec<String>,
}

// ------------------- Defaults -------------------

fn default_limit() -> usize {
    10
}

// ------------------- Main -------------------

#[tokio::main]
async fn main() {
    dotenv().ok();

    let github_token =
        std::env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN environment variable not set");

    let client = Client::builder()
        .user_agent("Sui-Move-Users-Fetcher")
        .build()
        .expect("Failed to build reqwest client");

    let app = Router::new()
        .route("/", get(root))
        .route("/sui-move-users", get(get_sui_move_users))
        .route("/check-sui-developer", get(check_sui_developer_handler))
        .layer(Extension(client))
        .layer(Extension(github_token));

    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let listener = TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("Failed to bind port");

    println!("🚀 Server running on http://0.0.0.0:{port}");
    axum::serve(listener, app).await.unwrap();
}

// ------------------- Handlers -------------------

async fn root() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "service": "Sui Move GitHub Users API",
        "endpoints": {
            "/sui-move-users": "Get GitHub users who have written Sui Move code",
            "/check-sui-developer?username=<github_user>": "Check if a specific GitHub user has .move files"
        },
        "example": "/sui-move-users?limit=10"
    }))
}

async fn check_sui_developer_handler(
    Query(params): Query<DeveloperQuery>,
    Extension(client): Extension<Client>,
    Extension(token): Extension<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let username = &params.username;

    match user_has_move_file(&client, &token, username).await {
        Ok(true) => Ok(Json(serde_json::json!({"username": username, "has_move_files": true}))),
        Ok(false) => Ok(Json(serde_json::json!({"username": username, "has_move_files": false}))),
        Err(e) => Err((StatusCode::BAD_GATEWAY, e.to_string())),
    }
}

// ------------------- Helper Functions -------------------

async fn user_has_move_file(
    client: &Client,
    token: &str,
    username: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let url = format!(
        "https://api.github.com/search/code?q=extension:move+user:{}&per_page=1",
        username
    );

    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "Move-Scanner")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await?;

    if let Err(e) = resp.error_for_status_ref() {
        let body = resp.text().await.unwrap_or_else(|_| "<failed to read body>".to_string());
        let status_str = e.status()
            .map(|s| s.as_u16().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        return Err(format!("GitHub API error {}: {}", status_str, body).into());
    }

    let result: CodeSearchResult = resp.json().await?;
    Ok(matches!(result.total_count, Some(c) if c > 0))
}

// ------------------- Sui Move Users -------------------

async fn get_sui_move_users(
    Query(params): Query<SearchParams>,
) -> Result<Json<Vec<UserResponse>>, (StatusCode, String)> {
    let github_token = std::env::var("GITHUB_TOKEN").map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "GitHub token not configured on server".to_string(),
        )
    })?;

    let client = Client::builder()
        .user_agent("Sui-Move-Users-Fetcher")
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Search for .move files using GitHub Code Search API
    let search_queries = vec!["extension:move"];
    let mut all_repos: HashSet<String> = HashSet::new();

    for query in search_queries {
        for page in 1..=10 {
            let url = format!(
                "https://api.github.com/search/code?q={}&per_page=100&page={}",
                urlencoding::encode(query),
                page
            );

            let response = client
                .get(&url)
                .header("Accept", "application/vnd.github+json")
                .header("Authorization", format!("Bearer {}", github_token))
                .header("X-GitHub-Api-Version", "2022-11-28")
                .send()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, format!("GitHub API error: {}", e)))?;

            if !response.status().is_success() {
                let status = response.status();
                let error_text = response.text().await.unwrap_or_default();
                if status == 403 {
                    return Err((
                        StatusCode::TOO_MANY_REQUESTS,
                        format!("GitHub API rate limit exceeded. Error: {}", error_text),
                    ));
                }
                return Err((
                    StatusCode::BAD_GATEWAY,
                    format!("GitHub API returned status {}: {}", status, error_text),
                ));
            }

            let search_result: GitHubCodeSearchResponse = response
                .json()
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            if search_result.items.is_empty() {
                break;
            }

            for item in search_result.items {
                all_repos.insert(item.repository.full_name);
            }

            if all_repos.len() >= params.limit * 3 {
                break;
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(6)).await;
        }

        if all_repos.len() >= params.limit * 3 {
            break;
        }
    }

    let mut user_data: HashMap<String, UserResponse> = HashMap::new();
    let mut repos_processed = 0;

    for repo_name in all_repos.iter().take(params.limit * 2) {
        let url = format!("https://api.github.com/repos/{}/contributors", repo_name);

        let response = client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", format!("Bearer {}", github_token))
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await;

        if let Ok(resp) = response {
            if resp.status().is_success() {
                if let Ok(contributors) = resp.json::<Vec<Contributor>>().await {
                    for contributor in contributors {
                        user_data
                            .entry(contributor.login.clone())
                            .and_modify(|u| {
                                u.total_contributions += contributor.contributions;
                                if !u.repositories.contains(repo_name) {
                                    u.repositories.push(repo_name.clone());
                                }
                            })
                            .or_insert(UserResponse {
                                login: contributor.login.clone(),
                                avatar_url: contributor.avatar_url.clone(),
                                profile_url: contributor.html_url.clone(),
                                total_contributions: contributor.contributions,
                                repositories: vec![repo_name.clone()],
                            });
                    }
                    repos_processed += 1;
                }
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;
    }

    let mut users: Vec<UserResponse> = user_data.into_values().collect();
    users.sort_by(|a, b| b.total_contributions.cmp(&a.total_contributions));
    let result = users.into_iter().take(params.limit).collect();
    Ok(Json(result))
}
