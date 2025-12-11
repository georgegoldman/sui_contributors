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
    total_count: Option<u32>,
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

#[derive(Debug, Deserialize)]
struct Commit {
    commit: CommitDetails,
    author: Option<CommitAuthor>,
}

#[derive(Debug, Deserialize)]
struct CommitDetails {
    author: CommitUserDetails,
}

#[derive(Debug, Deserialize)]
struct CommitUserDetails {
    name: String,
    email: String,
}

#[derive(Debug, Deserialize)]
struct CommitAuthor {
    login: String,
}

#[derive(Debug, Serialize)]
struct UserResponse {
    login: String,
    avatar_url: String,
    profile_url: String,
    total_contributions: u32,
    repositories: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RepositoryWithCommits {
    repo_name: String,
    repo_url: String,
    commit_count: u32,
}

#[derive(Debug, Serialize)]
struct UserMoveFilesResponse {
    username: String,
    has_move_files: bool,
    total_repositories: usize,
    total_commits: u32,
    repositories: Vec<RepositoryWithCommits>,
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
            "/check-sui-developer?username=<github_user>": "Check if a specific GitHub user has .move files with repo and commit details"
        },
        "example": "/sui-move-users?limit=10"
    }))
}

async fn check_sui_developer_handler(
    Query(params): Query<DeveloperQuery>,
    Extension(client): Extension<Client>,
    Extension(token): Extension<String>,
) -> Result<Json<UserMoveFilesResponse>, (StatusCode, String)> {
    let username = &params.username;

    match get_user_move_repos(&client, &token, username).await {
        Ok(response) => Ok(Json(response)),
        Err(e) => Err((StatusCode::BAD_GATEWAY, e.to_string())),
    }
}

// ------------------- Helper Functions -------------------

async fn get_user_move_repos(
    client: &Client,
    token: &str,
    username: &str,
) -> Result<UserMoveFilesResponse, Box<dyn std::error::Error>> {
    // Step 1: Get ALL user repositories (not limited by code search)
    let mut all_user_repos = Vec::new();
    let mut page = 1;
    
    loop {
        let url = format!(
            "https://api.github.com/users/{}/repos?per_page=100&page={}",
            username, page
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

        let repos: Vec<Repository> = resp.json().await?;
        
        if repos.is_empty() {
            break;
        }

        all_user_repos.extend(repos);
        page += 1;
        
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    // Step 2: Check each repository for .move files
    let mut repos_with_move_files = Vec::new();
    
    for repo in all_user_repos {
        if repo_has_move_files(client, token, &repo.full_name).await? {
            repos_with_move_files.push(repo.full_name);
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
    }

    // Step 3: Get commit counts for repositories with .move files
    let mut repositories_with_commits = Vec::new();
    let mut total_commits = 0u32;

    for repo_name in &repos_with_move_files {
        let commit_count = get_user_commit_count(client, token, repo_name, username).await?;
        
        repositories_with_commits.push(RepositoryWithCommits {
            repo_name: repo_name.clone(),
            repo_url: format!("https://github.com/{}", repo_name),
            commit_count,
        });
        
        total_commits += commit_count;
        
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    // Sort by commit count descending
    repositories_with_commits.sort_by(|a, b| b.commit_count.cmp(&a.commit_count));

    Ok(UserMoveFilesResponse {
        username: username.to_string(),
        has_move_files: !repositories_with_commits.is_empty(),
        total_repositories: repositories_with_commits.len(),
        total_commits,
        repositories: repositories_with_commits,
    })
}

async fn repo_has_move_files(
    client: &Client,
    token: &str,
    repo_name: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    // Use the Git Trees API to recursively check for .move files
    // First, get the default branch
    let repo_url = format!("https://api.github.com/repos/{}", repo_name);
    
    let repo_resp = client
        .get(&repo_url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "Move-Scanner")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await?;

    if !repo_resp.status().is_success() {
        return Ok(false);
    }

    let repo_data: serde_json::Value = repo_resp.json().await?;
    let default_branch = repo_data["default_branch"]
        .as_str()
        .unwrap_or("main");

    // Get the tree recursively
    let tree_url = format!(
        "https://api.github.com/repos/{}/git/trees/{}?recursive=1",
        repo_name, default_branch
    );

    let tree_resp = client
        .get(&tree_url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "Move-Scanner")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await?;

    if !tree_resp.status().is_success() {
        return Ok(false);
    }

    let tree_data: serde_json::Value = tree_resp.json().await?;
    
    // Check if any file ends with .move
    if let Some(tree) = tree_data["tree"].as_array() {
        for item in tree {
            if let Some(path) = item["path"].as_str() {
                if path.ends_with(".move") {
                    return Ok(true);
                }
            }
        }
    }

    Ok(false)
}

async fn get_user_commit_count(
    client: &Client,
    token: &str,
    repo_name: &str,
    username: &str,
) -> Result<u32, Box<dyn std::error::Error>> {
    let mut total_commits = 0u32;
    let mut page = 1;

    loop {
        let url = format!(
            "https://api.github.com/repos/{}/commits?author={}&per_page=100&page={}",
            repo_name, username, page
        );

        let resp = client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "Move-Scanner")
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?;

        if !resp.status().is_success() {
            // If we can't get commits (e.g., repo is private or deleted), return 0
            break;
        }

        let commits: Vec<Commit> = resp.json().await?;
        
        // Break when no more commits
        if commits.is_empty() {
            break;
        }

        total_commits += commits.len() as u32;
        page += 1;

        // Continue fetching ALL pages until exhausted (no artificial limit)
    }

    Ok(total_commits)
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