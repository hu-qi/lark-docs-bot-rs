//! Manual Feishu API integration smoke test.
//! Creates one clearly labelled disposable document in the configured tenant.
//! Does not print API credentials, tenant tokens or document content.
use reqwest::{Client, Method};
use serde_json::{json, Value};
use std::{env, process, time::Duration};

fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        eprintln!("Missing required environment variable: {name}");
        process::exit(2)
    })
}

async fn call(client: &Client, method: Method, url: &str, token: Option<&str>, body: Option<Value>) -> Result<Value, String> {
    let mut request = client.request(method, url);
    if let Some(token) = token { request = request.bearer_auth(token); }
    if let Some(body) = body { request = request.json(&body); }
    let response = request.send().await.map_err(|e| format!("Network failure: {e}"))?;
    let status = response.status();
    let payload: Value = response.json().await.map_err(|e| format!("Malformed Feishu response: {e}"))?;
    if !status.is_success() || payload["code"].as_i64() != Some(0) {
        return Err(format!("Feishu HTTP {} code={} message={}",
            status,
            payload["code"].as_i64().unwrap_or(-1),
            payload["msg"].as_str().unwrap_or("unknown")));
    }
    Ok(payload["data"].clone())
}

#[tokio::main]
async fn main() {
    if let Err(e) = verify().await {
        eprintln!("Feishu smoke test failed: {e}");
        process::exit(1);
    }
}
async fn verify() -> Result<(), String> {
    let app_id = required("LARK_APP_ID");
    let app_secret = required("LARK_APP_SECRET");
    let base = env::var("LARK_API_BASE").unwrap_or_else(|_| "https://open.feishu.cn/open-apis".into());
    let client = Client::builder().timeout(Duration::from_secs(30)).build().map_err(|e| e.to_string())?;

    let auth = call(&client, Method::POST, &format!("{base}/auth/v3/tenant_access_token/internal"), None,
        Some(json!({"app_id":app_id,"app_secret":app_secret}))).await?;
    let token = auth["tenant_access_token"].as_str().ok_or("Missing tenant token")?;
    println!("STEP 1/4: tenant_access_token granted");

    // Running this creates a new document in the application's default location.
    let marker = format!("Lark Docs Bot API Smoke {}", env::var("GITHUB_RUN_ID").unwrap_or_else(|_| "manual".into()));
    let created = call(&client, Method::POST, &format!("{base}/docx/v1/documents"), Some(token),
        Some(json!({"title":marker}))).await?;
    let id = created["document"]["document_id"].as_str().ok_or("Missing document_id")?;
    println!("STEP 2/4: create document OK (id: {id})");
    println!("Test document URL: https://www.feishu.cn/docx/{id}");

    let text = "Automated integration test: 飞书文档读写验证";
    let inserted = call(&client, Method::POST,
        &format!("{base}/docx/v1/documents/{id}/blocks/{id}/children"), Some(token),
        Some(json!({"children":[{"block_type":2,"text":{"elements":[{"text_run":{"content":text}}]}}]}))).await?;
    let block_id = inserted["children"][0]["block_id"].as_str().ok_or("Missing created block_id")?;
    println!("STEP 3/4: append text block OK");

    let read = call(&client, Method::GET, &format!("{base}/docx/v1/documents/{id}/raw_content"), Some(token), None).await?;
    let content = read["content"].as_str().ok_or("Missing raw_content")?;
    if !content.contains(text) { return Err("Readback did not contain expected text".into()); }
    println!("STEP 4/4: readback content verified");
    println!("Smoke test succeeded. Document and block IDs: {id} / {block_id}");
    Ok(())
}
