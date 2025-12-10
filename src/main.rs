use axum::{
    extract::Query,
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tokio::net::TcpListener;
use dotenv::dotenv;

// Remove github_token from SearchParams
#[derive(Debug, Deserialize)]
struct SearchParams {
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    10
}

#[tokio::main]
async fn main() {
    dotenv().ok();
    // Validate token exists at startup
    if std::env::var("GITHUB_TOKEN").is_err() {
        eprintln!("❌ ERROR: GITHUB_TOKEN environment variable not set!");
        eprintln!("   Please set it before running the server:");
        eprintln!("   export GITHUB_TOKEN=your_token_here");
        std::process::exit(1);
    }

    let app = Router::new()
        .route("/", get(root))
        .route("/sui-move-users", get(get_sui_move_users));

    let listener = TcpListener::bind("127.0.0.1:3000").await.unwrap();
    println!("🚀 Server running on http://127.0.0.1:3000");
    println!("📍 Endpoints:");
    println!("   GET / - API info");
    println!("   GET /sui-move-users?limit=10 - Fetch Sui Move developers");
    println!("✅ GitHub token loaded from environment");
    
    axum::serve(listener, app).await.unwrap();
}

async fn root() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "service": "Sui Move GitHub Users API",
        "endpoints": {
            "/sui-move-users": "Get GitHub users who have written Sui Move code",
        },
        "example": "/sui-move-users?limit=10"
    }))
}

async fn get_sui_move_users(
    Query(params): Query<SearchParams>,
) -> Result<Json<Vec<UserResponse>>, (StatusCode, String)> {
    // Get GitHub token from environment only
    let github_token = std::env::var("GITHUB_TOKEN")
        .map_err(|_| (
            StatusCode::INTERNAL_SERVER_ERROR,
            "GitHub token not configured on server".to_string(),
        ))?;

    let client = reqwest::Client::builder()
        .user_agent("Sui-Move-Users-Fetcher")
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // ... rest of your code stays the same
    
    // Search for .move files using GitHub Code Search API
    let search_queries = vec![
        "extension:move",
        // "module extension:move",
        // "sui extension:move",
    ];

    let mut all_repos: HashSet<String> = HashSet::new();
    
    for query in search_queries {
        println!("🔍 Searching for: {}", query);
        
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

    println!("✅ Found {} unique repositories with .move files", all_repos.len());

    let mut user_data: HashMap<String, UserResponse> = HashMap::new();
    let mut repos_processed = 0;

    for repo_name in all_repos.iter().take(params.limit * 2) {
        println!("📊 Fetching contributors for: {}", repo_name);
        
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

    println!("✅ Processed {} repositories", repos_processed);
    println!("✅ Found {} unique contributors", user_data.len());

    let mut users: Vec<UserResponse> = user_data.into_values().collect();
    users.sort_by(|a, b| b.total_contributions.cmp(&a.total_contributions));

    let result = users.into_iter().take(params.limit).collect();

    Ok(Json(result))
}

// Keep your struct definitions
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