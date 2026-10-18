use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, env, path::PathBuf, sync::Arc};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use tower_http::{limit::RequestBodyLimitLayer, trace::TraceLayer};

type ApiResult<T> = Result<T, (StatusCode, Json<Value>)>;

#[derive(Clone)]
struct AppState {
    http: reqwest::Client,
    config: Arc<Config>,
    registry: Arc<Mutex<BTreeMap<String, DocEntry>>>,
    token: Arc<Mutex<Option<Token>>>,
}
struct Config {
    app_id: String,
    app_secret: String,
    admin_key: String,
    write_key: String,
    api_base: String,
    doc_base: String,
    data_file: PathBuf,
}
#[derive(Clone)]
struct Token { value: String, expires: std::time::Instant }
#[derive(Clone, Serialize, Deserialize)]
struct DocEntry { id: String, title: String, public_read: bool }
#[derive(Deserialize)]
struct CreateDoc { title: String }
#[derive(Deserialize)]
struct AppendText { text: String }
#[derive(Deserialize)]
struct ReplaceBlock { text: String }
#[derive(Deserialize)]
struct AccessChange { public_read: bool }

fn problem(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": message.into()})))
}
fn equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ")
}
fn role(headers: &HeaderMap, c: &Config) -> u8 {
    let key = match bearer(headers) { Some(k) => k, None => return 0 };
    if equal(key, &c.admin_key) { 2 } else if equal(key, &c.write_key) { 1 } else { 0 }
}
fn require(headers: &HeaderMap, c: &Config, needed: u8) -> ApiResult<()> {
    if role(headers, c) >= needed { Ok(()) } else {
        Err(problem(StatusCode::UNAUTHORIZED, "Valid Bearer API key required"))
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
async fn saved(state: &AppState) -> ApiResult<()> {
    let contents = serde_json::to_vec_pretty(&*state.registry.lock().await)
        .map_err(|_| problem(StatusCode::INTERNAL_SERVER_ERROR, "Serialize registry"))?;
    let path = &state.config.data_file;
    let temp = path.with_extension("tmp");
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(|e| problem(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    tokio::fs::write(&temp, contents).await.map_err(|e| problem(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tokio::fs::rename(&temp, path).await.map_err(|e| problem(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(())
}
async fn allowed(state: &AppState, headers: &HeaderMap, id: &str, write: bool) -> ApiResult<DocEntry> {
    if !valid_id(id) { return Err(problem(StatusCode::BAD_REQUEST, "Invalid document ID")); }
    let doc = state.registry.lock().await.get(id).cloned()
        .ok_or_else(|| problem(StatusCode::NOT_FOUND, "Document not registered"))?;
    if write { require(headers, &state.config, 1)?; }
    else if !doc.public_read { require(headers, &state.config, 1)?; }
    Ok(doc)
}
async fn lark_token(state: &AppState) -> ApiResult<String> {
    let mut cache = state.token.lock().await;
    if let Some(ref token) = *cache {
        if token.expires > std::time::Instant::now() { return Ok(token.value.clone()); }
    }
    let res = state.http.post(format!("{}/auth/v3/tenant_access_token/internal", state.config.api_base))
        .json(&json!({"app_id":state.config.app_id,"app_secret":state.config.app_secret}))
        .send().await.map_err(upstream_error)?;
    let data: Value = res.json().await.map_err(upstream_error)?;
    if data["code"].as_i64() != Some(0) {
        tracing::warn!("Feishu authentication failed: {}", data["code"]);
        return Err(problem(StatusCode::BAD_GATEWAY, "Feishu authentication failed; check application configuration"));
    }
    let value = data["tenant_access_token"].as_str().ok_or_else(|| problem(StatusCode::BAD_GATEWAY, "Missing tenant token"))?.to_owned();
    let duration = data["expire"].as_u64().unwrap_or(7200).saturating_sub(120).max(60);
    *cache = Some(Token { value: value.clone(), expires: std::time::Instant::now() + std::time::Duration::from_secs(duration) });
    Ok(value)
}
fn upstream_error(e: reqwest::Error) -> (StatusCode, Json<Value>) {
    tracing::warn!("Feishu upstream request error: {}", e);
    problem(StatusCode::BAD_GATEWAY, "Feishu API unavailable")
}
async fn lark(state: &AppState, method: reqwest::Method, path: &str, payload: Option<Value>) -> ApiResult<Value> {
    let token = lark_token(state).await?;
    let mut request = state.http.request(method, format!("{}{}", state.config.api_base, path)).bearer_auth(token);
    if let Some(value) = payload { request = request.json(&value); }
    let response = request.send().await.map_err(upstream_error)?;
    let status = response.status();
    let body: Value = response.json().await.map_err(upstream_error)?;
    if !status.is_success() || body["code"].as_i64() != Some(0) {
        let code = body["code"].as_i64().unwrap_or(-1);
        tracing::warn!("Feishu API rejected: path={} status={} code={}", path, status, code);
        return Err(problem(StatusCode::BAD_GATEWAY, format!("Feishu API rejected operation (code {code})")));
    }
    Ok(body["data"].clone())
}
fn doc_url(c: &Config, id: &str) -> String {
    format!("{}/docx/{}", c.doc_base.trim_end_matches('/'), id)
}
async fn create_internal(state: &AppState, title: &str) -> ApiResult<Value> {
    if title.trim().is_empty() || title.chars().count() > 200 {
        return Err(problem(StatusCode::BAD_REQUEST, "Title must contain 1-200 characters"));
    }
    let data = lark(state, reqwest::Method::POST, "/docx/v1/documents", Some(json!({"title":title}))).await?;
    let id = data["document"]["document_id"].as_str()
        .ok_or_else(|| problem(StatusCode::BAD_GATEWAY, "Feishu response missing document_id"))?.to_string();
    let doc = DocEntry { id: id.clone(), title: title.to_owned(), public_read: false };
    state.registry.lock().await.insert(id.clone(), doc.clone());
    saved(state).await?;
    Ok(json!({"id":id,"title":title,"url":doc_url(&state.config,&id),"public_read":false}))
}
async fn append_internal(state: &AppState, id: &str, text: &str) -> ApiResult<Value> {
    if text.is_empty() || text.chars().count() > 20000 || text.split('\n').count() > 50 {
        return Err(problem(StatusCode::BAD_REQUEST, "Text must contain 1-20000 characters and at most 50 paragraphs"));
    }
    let blocks: Vec<Value> = text.split('\n').map(|line| json!({
        "block_type":2,
        "text":{"elements":[{"text_run":{"content":line}}]}
    })).collect();
    lark(state, reqwest::Method::POST, &format!("/docx/v1/documents/{id}/blocks/{id}/children"), Some(json!({"children":blocks}))).await
}
async fn health() -> Json<Value> { Json(json!({"status":"ok"})) }
async fn index() -> Html<&'static str> { Html(include_str!("index.html")) }
async fn list_docs(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    require(&headers, &s.config, 1)?;
    let docs: Vec<Value> = s.registry.lock().await.values().map(|d| json!({
        "id": d.id,"title":d.title,"public_read":d.public_read,"url":doc_url(&s.config,&d.id)
    })).collect();
    Ok(Json(json!({"documents":docs})))
}
async fn create_doc(State(s): State<AppState>, headers: HeaderMap, Json(req): Json<CreateDoc>) -> ApiResult<Json<Value>> {
    require(&headers, &s.config, 2)?;
    Ok(Json(create_internal(&s, &req.title).await?))
}
async fn get_doc(State(s): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let doc = allowed(&s, &headers, &id, false).await?;
    let data = lark(&s, reqwest::Method::GET, &format!("/docx/v1/documents/{id}/raw_content"), None).await?;
    Ok(Json(json!({"document":doc,"content":data["content"],"url":doc_url(&s.config,&id)})))
}
async fn blocks(State(s): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    allowed(&s, &headers, &id, false).await?;
    let data = lark(&s, reqwest::Method::GET, &format!("/docx/v1/documents/{id}/blocks?page_size=100"), None).await?;
    Ok(Json(data))
}
async fn append(State(s): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(req): Json<AppendText>) -> ApiResult<Json<Value>> {
    allowed(&s, &headers, &id, true).await?;
    Ok(Json(append_internal(&s, &id, &req.text).await?))
}
async fn replace_block(State(s): State<AppState>, headers: HeaderMap, Path((id, block_id)): Path<(String, String)>, Json(req): Json<ReplaceBlock>) -> ApiResult<Json<Value>> {
    allowed(&s, &headers, &id, true).await?;
    if !valid_id(&block_id) || block_id == id {
        return Err(problem(StatusCode::BAD_REQUEST, "Invalid editable block ID"));
    }
    if req.text.chars().count() > 20000 {
        return Err(problem(StatusCode::BAD_REQUEST, "Block text exceeds 20000 characters"));
    }
    // Limit modification to plain text blocks. Do not silently overwrite
    // embedded images, mentions, tables or unsupported rich content.
    let block = lark(&s, reqwest::Method::GET, &format!("/docx/v1/documents/{id}/blocks/{block_id}"), None).await?;
    let block_data = &block["block"];
    if block_data["block_type"].as_i64() != Some(2) {
        return Err(problem(StatusCode::BAD_REQUEST, "Only plain text blocks can be edited"));
    }
    let elements = block_data["text"]["elements"].as_array()
        .ok_or_else(|| problem(StatusCode::BAD_REQUEST, "Block has unsupported content"))?;
    if elements.iter().any(|element| element.get("text_run").is_none()) {
        return Err(problem(StatusCode::BAD_REQUEST, "Refusing to overwrite mixed rich content"));
    }
    let data = lark(&s, reqwest::Method::PATCH, &format!("/docx/v1/documents/{id}/blocks/{block_id}"),
        Some(json!({"update_text_elements":{"elements":[{"text_run":{"content":req.text}}]}}))).await?;
    Ok(Json(data))
}
async fn set_access(State(s): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(req): Json<AccessChange>) -> ApiResult<Json<Value>> {
    require(&headers, &s.config, 2)?;
    let mut registry = s.registry.lock().await;
    let doc = registry.get_mut(&id).ok_or_else(|| problem(StatusCode::NOT_FOUND, "Document not registered"))?;
    doc.public_read = req.public_read;
    let result = json!({"id":id,"public_read":req.public_read});
    drop(registry);
    saved(&s).await?;
    Ok(Json(result))
}
fn mcp_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn mcp_result(id: Value, val: Value) -> Value { json!({"jsonrpc":"2.0","id":id,"result":val}) }
async fn mcp(State(s): State<AppState>, headers: HeaderMap, Json(message): Json<Value>) -> impl IntoResponse {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message["method"].as_str().unwrap_or("");
    if method == "notifications/initialized" { return StatusCode::ACCEPTED.into_response(); }
    if id.is_null() { return (StatusCode::BAD_REQUEST, Json(mcp_error(id,-32600,"Missing JSON-RPC request id"))).into_response(); }
    let answer = match method {
        "initialize" => mcp_result(id, json!({
            "protocolVersion":"2025-03-26",
            "capabilities":{"tools":{}},
            "serverInfo":{"name":"lark-docs-bot-rs","version":"0.1.0"}
        })),
        "ping" => mcp_result(id, json!({})),
        "tools/list" => mcp_result(id, json!({"tools":[
            {"name":"list_documents","description":"List documents registered in this gateway","inputSchema":{"type":"object","properties":{}}},
            {"name":"create_document","description":"Create a new Feishu document","inputSchema":{"type":"object","properties":{"title":{"type":"string"}},"required":["title"]}},
            {"name":"read_document","description":"Read registered document text","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}},
            {"name":"append_text","description":"Append text to a registered document","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"text":{"type":"string"}},"required":["id","text"]}}
        ]})),
        "tools/call" => {
            let args = &message["params"]["arguments"];
            let name = message["params"]["name"].as_str().unwrap_or("");
            let op: ApiResult<Value> = async {
                match name {
                    "list_documents" => {
                        require(&headers, &s.config, 1)?;
                        let docs: Vec<DocEntry> = s.registry.lock().await.values().cloned().collect();
                        Ok(json!(docs))
                    }
                    "create_document" => {
                        require(&headers, &s.config, 2)?;
                        create_internal(&s, args["title"].as_str().unwrap_or("")).await
                    }
                    "read_document" => {
                        let doc_id = args["id"].as_str().unwrap_or("");
                        allowed(&s, &headers, doc_id, false).await?;
                        lark(&s, reqwest::Method::GET, &format!("/docx/v1/documents/{doc_id}/raw_content"), None).await
                    }
                    "append_text" => {
                        let doc_id = args["id"].as_str().unwrap_or("");
                        allowed(&s, &headers, doc_id, true).await?;
                        append_internal(&s, doc_id, args["text"].as_str().unwrap_or("")).await
                    }
                    _ => Err(problem(StatusCode::BAD_REQUEST,"Unknown tool")),
                }
            }.await;
            match op {
                Ok(value) => mcp_result(id, json!({"content":[{"type":"text","text":value.to_string()}]})),
                Err((status, Json(value))) => mcp_result(id, json!({"isError":true,"content":[{"type":"text","text":format!("{}: {}", status, value["error"].as_str().unwrap_or("Error"))}]}))
            }
        }
        _ => mcp_error(id,-32601,"Method not found"),
    };
    (StatusCode::OK, Json(answer)).into_response()
}
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let required = |name: &str| env::var(name).unwrap_or_else(|_| panic!("Missing required environment variable: {name}"));
    let config = Config {
        app_id: required("LARK_APP_ID"),
        app_secret: required("LARK_APP_SECRET"),
        admin_key: required("ADMIN_API_KEY"),
        write_key: required("WRITE_API_KEY"),
        api_base: env::var("LARK_API_BASE").unwrap_or_else(|_| "https://open.feishu.cn/open-apis".into()),
        doc_base: env::var("LARK_DOC_BASE").unwrap_or_else(|_| "https://www.feishu.cn".into()),
        data_file: env::var("DATA_FILE").unwrap_or_else(|_| "data/documents.json".into()).into(),
    };
    assert!(config.admin_key.len() >= 32 && config.write_key.len() >= 32 && config.admin_key != config.write_key, "Use distinct API keys of at least 32 characters");
    let bind = env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let registry: BTreeMap<String, DocEntry> = match tokio::fs::read(&config.data_file).await {
        Ok(contents) => serde_json::from_slice(&contents).expect("Invalid registry JSON"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => panic!("Cannot read registry: {e}"),
    };
    let state = AppState {
        http: reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().unwrap(),
        config: Arc::new(config),
        registry: Arc::new(Mutex::new(registry)),
        token: Arc::new(Mutex::new(None)),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/api/docs", get(list_docs).post(create_doc))
        .route("/api/docs/{id}", get(get_doc))
        .route("/api/docs/{id}/blocks", get(blocks))
        .route("/api/docs/{id}/append", post(append))
        .route("/api/docs/{id}/blocks/{block_id}", axum::routing::patch(replace_block))
        .route("/api/docs/{id}/access", post(set_access))
        .route("/mcp", post(mcp))
        .layer(RequestBodyLimitLayer::new(1024 * 256))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("Bind failed");
    tracing::info!("Lark Docs Bot listening on {bind}");
    axum::serve(listener, app).await.unwrap();
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn id_validation() {
        assert!(valid_id("docx_abc-123"));
        assert!(!valid_id("../secret"));
        assert!(!valid_id(""));
    }
    #[test] fn timing_safe_equal() {
        assert!(equal("somekey", "somekey"));
        assert!(!equal("somekey", "anotherkey"));
    }
}
