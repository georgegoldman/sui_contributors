use axum::{
    extract::Query, http::{StatusCode, header::{AUTHORIZATION, CONTENT_TYPE}}, response::Json, routing::get, Extension, Router,
};
use tower_http::cors::{Any, CorsLayer};
use dotenv::dotenv;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tokio::net::TcpListener;

// ------------------- Structs -------------------

#[derive(Debug, Deserialize)]
struct DeveloperQuery {
    username: String,
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

    let app_cors = CorsLayer::new()
    .allow_methods([Method::GET, Method::POST])
    .allow_origin(Any);

    let app = Router::new()
        .route("/", get(root))
        .route("/check-sui-developer", get(check_sui_developer_handler))
        .layer(Extension(client))
        .layer(app_cors)
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
            "/check-sui-developer?username=<github_user>": "Check if a specific GitHub user has .move files with repo and commit details"
        },
        "example": "/check-sui-developer?username=dotandev"
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

// ------------------- GraphQL Helper -------------------

async fn graphql_request(
    client: &Client,
    token: &str,
    query: &str,
    variables: Option<serde_json::Value>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut body = serde_json::json!({ "query": query });
    if let Some(vars) = variables {
        body["variables"] = vars;
    }

    let resp = client
        .post("https://api.github.com/graphql")
        .header("Authorization", format!("Bearer {}", token))
        .header("User-Agent", "Sui-Move-Users-Fetcher")
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = resp.json().await?;
    if let Some(errors) = json.get("errors") {
        return Err(format!("GraphQL errors: {}", errors).into());
    }

    Ok(json["data"].clone())
}

// ------------------- Core Logic -------------------

async fn get_user_move_repos(
    client: &Client,
    token: &str,
    username: &str,
) -> Result<UserMoveFilesResponse, Box<dyn std::error::Error>> {
    // Step 1: Fetch repositories via GraphQL
    let mut repositories = Vec::new();
    let mut after: Option<String> = None;

    let query = r#"
    query($login:String!, $after:String) {
      user(login:$login) {
        repositories(first:50, after:$after, ownerAffiliations:OWNER, isFork:false) {
          nodes {
            nameWithOwner
            url
            defaultBranchRef { name }
          }
          pageInfo { hasNextPage endCursor }
        }
      }
    }
    "#;

    loop {
        let vars = serde_json::json!({ "login": username, "after": after });
        let data = graphql_request(client, token, query, Some(vars)).await?;

        if let Some(nodes) = data["user"]["repositories"]["nodes"].as_array() {
            for node in nodes {
                let name = node["nameWithOwner"].as_str().unwrap_or_default().to_string();
                let url = node["url"].as_str().unwrap_or_default().to_string();
                let branch = node["defaultBranchRef"]["name"].as_str().unwrap_or("main").to_string();

                repositories.push((name, url, branch));
            }
        }

        let page_info = &data["user"]["repositories"]["pageInfo"];
        let has_next = page_info["hasNextPage"].as_bool().unwrap_or(false);
        after = page_info["endCursor"].as_str().map(|s| s.to_string());

        if !has_next {
            break;
        }

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    // Step 2: Check for .move files in each repo via REST Git Trees API
    let mut repos_with_move = Vec::new();
    for (name, url, branch) in &repositories {
        let tree_url = format!("https://api.github.com/repos/{}/git/trees/{}?recursive=1", name, branch);
        let resp = client
            .get(&tree_url)
            .header("Authorization", format!("Bearer {}", token))
            .header("User-Agent", "Sui-Move-Users-Fetcher")
            .send()
            .await?;

        if resp.status().is_success() {
            let tree: serde_json::Value = resp.json().await?;
            if let Some(items) = tree["tree"].as_array() {
                if items.iter().any(|f| f["path"].as_str().map(|p| p.ends_with(".move")).unwrap_or(false)) {
                    repos_with_move.push((name.clone(), url.clone()));
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    // Step 3: Count commits for each repo with .move files
    let mut total_commits = 0u32;
    let mut repositories_with_commits = Vec::new();

    for (name, url) in &repos_with_move {
        let mut page = 1;
        let mut repo_commits = 0u32;

        loop {
            let commits_url = format!("https://api.github.com/repos/{}/commits?author={}&per_page=100&page={}", name, username, page);
            let resp = client
                .get(&commits_url)
                .header("Authorization", format!("Bearer {}", token))
                .header("User-Agent", "Sui-Move-Users-Fetcher")
                .send()
                .await?;

            if !resp.status().is_success() { break; }

            let commits: Vec<serde_json::Value> = resp.json().await.unwrap_or_default();
            if commits.is_empty() { break; }

            repo_commits += commits.len() as u32;
            page += 1;
        }

        repositories_with_commits.push(RepositoryWithCommits {
            repo_name: name.clone(),
            repo_url: url.clone(),
            commit_count: repo_commits,
        });

        total_commits += repo_commits;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    repositories_with_commits.sort_by(|a, b| b.commit_count.cmp(&a.commit_count));

    Ok(UserMoveFilesResponse {
        username: username.to_string(),
        has_move_files: !repositories_with_commits.is_empty(),
        total_repositories: repositories_with_commits.len(),
        total_commits,
        repositories: repositories_with_commits,
    })
}
