use axum::{
    extract::{Path, Query, State},
    body::Body,
    middleware::{self, Next},
    http::{header, HeaderMap, Request, StatusCode, Method},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, env, path::PathBuf, sync::Arc, time::{Duration, Instant}};
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
    rate_limits: Arc<Mutex<BTreeMap<String, RateBucket>>>,
}
struct RateBucket { started: Instant, count: u32 }
struct Config {
    app_id: String,
    app_secret: String,
    admin_key: String,
    write_key: String,
    api_base: String,
    doc_base: String,
    data_file: PathBuf,
    read_limit_per_minute: u32,
    write_limit_per_minute: u32,
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
struct AppendStructured { blocks: Vec<StructuredBlock> }
#[derive(Deserialize)]
struct StructuredBlock { kind: String, text: String, bold: Option<bool>, italic: Option<bool> }
#[derive(Deserialize)]
struct ReplaceBlock { text: String, expected_text: Option<String> }
#[derive(Deserialize)]
struct ReplaceRuns { runs: Vec<RunEdit> }
#[derive(Deserialize)]
struct RunEdit { expected_text: String, text: String }
#[derive(Deserialize)]
struct BlockPageQuery { page_token: Option<String> }
#[derive(Deserialize)]
struct DeleteBlockRequest { parent_id: String, block_id: String, index: usize, document_revision_id: i64 }
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
fn apply_bucket(bucket: &mut RateBucket, limit: u32, now: Instant) -> bool {
    if now.duration_since(bucket.started) >= Duration::from_secs(60) {
        bucket.started = now;
        bucket.count = 0;
    }
    if bucket.count >= limit { return false; }
    bucket.count += 1;
    true
}
async fn security_gate(State(state): State<AppState>, request: Request<Body>, next: Next) -> axum::response::Response {
    let path = request.uri().path().to_owned();
    if path == "/health" || path == "/" {
        return next.run(request).await;
    }
    let role = role(request.headers(), &state.config);
    let is_write = request.method() != Method::GET;
    let category = if role == 2 { "admin" } else if role == 1 { "writer" } else { "anonymous" };
    let bucket_key = format!("{category}:{}", if is_write { "write" } else { "read" });
    let limit = if is_write { state.config.write_limit_per_minute } else { state.config.read_limit_per_minute };
    let allowed = {
        let mut table = state.rate_limits.lock().await;
        let bucket = table.entry(bucket_key).or_insert_with(|| RateBucket { started: Instant::now(), count: 0 });
        apply_bucket(bucket, limit, Instant::now())
    };
    if !allowed {
        tracing::warn!(role = category, "request rate limit exceeded");
        return problem(StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded; retry in one minute").into_response();
    }
    let result = next.run(request).await;
    // Do not log request bodies, bearer credentials or document contents.
    if is_write {
        tracing::info!(role = category, operation = path, status = %result.status(), "document API mutation");
    }
    result
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
    if write || !doc.public_read { require(headers, &state.config, 1)?; }
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
fn encode_structured_blocks(blocks: &[StructuredBlock]) -> ApiResult<Vec<Value>> {
    if blocks.is_empty() || blocks.len() > 50 {
        return Err(problem(StatusCode::BAD_REQUEST, "Provide 1-50 structured blocks"));
    }
    let mut result = Vec::with_capacity(blocks.len());
    for block in blocks {
        if block.text.is_empty() || block.text.chars().count() > 2000 {
            return Err(problem(StatusCode::BAD_REQUEST, "Structured block text must contain 1-2000 characters"));
        }
        let (block_type, field) = match block.kind.as_str() {
            "text" => (2, "text"),
            "heading1" => (3, "heading1"),
            "heading2" => (4, "heading2"),
            "heading3" => (5, "heading3"),
            "bullet" => (12, "bullet"),
            "ordered" => (13, "ordered"),
            "quote" => (15, "quote"),
            _ => return Err(problem(StatusCode::BAD_REQUEST, "Unsupported structured block type")),
        };
        let mut run = json!({"content":block.text});
        if block.bold == Some(true) || block.italic == Some(true) {
            run["text_element_style"] = json!({"bold":block.bold.unwrap_or(false),"italic":block.italic.unwrap_or(false)});
        }
        let mut value = json!({"block_type":block_type});
        value[field] = json!({"elements":[{"text_run":run}]});
        result.push(value);
    }
    Ok(result)
}
async fn append_structured_internal(state: &AppState, id: &str, blocks: &[StructuredBlock]) -> ApiResult<Value> {
    let children = encode_structured_blocks(blocks)?;
    lark(state, reqwest::Method::POST, &format!("/docx/v1/documents/{id}/blocks/{id}/children"), Some(json!({"children":children}))).await
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
fn block_page_path(id: &str, token: Option<&str>) -> ApiResult<String> {
    let mut path = format!("/docx/v1/documents/{id}/blocks?page_size=100");
    if let Some(token) = token {
        if token.is_empty() || token.len() > 1024 || token.chars().any(char::is_control) {
            return Err(problem(StatusCode::BAD_REQUEST, "Invalid block page token"));
        }
        path.push_str("&page_token=");
        for byte in token.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                path.push(char::from(byte));
            } else {
                path.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    Ok(path)
}
async fn blocks(State(s): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Query(query): Query<BlockPageQuery>) -> ApiResult<Json<Value>> {
    allowed(&s, &headers, &id, false).await?;
    let path = block_page_path(&id, query.page_token.as_deref())?;
    let data = lark(&s, reqwest::Method::GET, &path, None).await?;
    Ok(Json(data))
}
async fn get_document_metadata(State(s): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    allowed(&s, &headers, &id, false).await?;
    Ok(Json(lark(&s, reqwest::Method::GET, &format!("/docx/v1/documents/{id}"), None).await?))
}
async fn get_children(State(s): State<AppState>, headers: HeaderMap, Path((id,parent_id)): Path<(String,String)>) -> ApiResult<Json<Value>> {
    allowed(&s, &headers, &id, false).await?;
    if !valid_id(&parent_id) {
        return Err(problem(StatusCode::BAD_REQUEST, "Invalid parent block ID"));
    }
    Ok(Json(lark(&s, reqwest::Method::GET, &format!("/docx/v1/documents/{id}/blocks/{parent_id}/children?page_size=500"), None).await?))
}
async fn append(State(s): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(req): Json<AppendText>) -> ApiResult<Json<Value>> {
    allowed(&s, &headers, &id, true).await?;
    Ok(Json(append_internal(&s, &id, &req.text).await?))
}
async fn append_structured(State(s): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(req): Json<AppendStructured>) -> ApiResult<Json<Value>> {
    allowed(&s, &headers, &id, true).await?;
    Ok(Json(append_structured_internal(&s, &id, &req.blocks).await?))
}
fn editable_text_run(block: &Value) -> ApiResult<Value> {
    let field = match block["block_type"].as_i64() {
        Some(2) => "text",
        Some(3) => "heading1", Some(4) => "heading2", Some(5) => "heading3",
        Some(12) => "bullet", Some(13) => "ordered", Some(15) => "quote",
        _ => return Err(problem(StatusCode::BAD_REQUEST, "Unsupported block type for text editing")),
    };
    let elements = block[field]["elements"].as_array()
        .ok_or_else(|| problem(StatusCode::BAD_REQUEST, "Block has no editable text elements"))?;
    if elements.len() != 1 || elements[0]["text_run"]["content"].as_str().is_none() {
        return Err(problem(StatusCode::BAD_REQUEST, "Only a single text run can be safely edited"));
    }
    Ok(elements[0].clone())
}
async fn replace_block_internal(s: &AppState, id: &str, block_id: &str, text: &str, expected_text: Option<&str>) -> ApiResult<Value> {
    if !valid_id(block_id) || block_id == id {
        return Err(problem(StatusCode::BAD_REQUEST, "Invalid editable block ID"));
    }
    if text.chars().count() > 20000 {
        return Err(problem(StatusCode::BAD_REQUEST, "Block text exceeds 20000 characters"));
    }
    let block = lark(s, reqwest::Method::GET, &format!("/docx/v1/documents/{id}/blocks/{block_id}"), None).await?;
    let mut run = editable_text_run(&block["block"])?;
    let previous = run["text_run"]["content"].as_str().unwrap_or("");
    if let Some(expected) = expected_text {
        if previous != expected {
            return Err(problem(StatusCode::CONFLICT, "Block content changed; reload before saving"));
        }
    }
    run["text_run"]["content"] = json!(text);
    // Reuse the original single run to retain its style and link attributes.
    lark(s, reqwest::Method::PATCH, &format!("/docx/v1/documents/{id}/blocks/{block_id}"),
        Some(json!({"update_text_elements":{"elements":[run]}}))).await
}
fn safe_run_elements(block: &Value) -> ApiResult<Vec<Value>> {
    let field = match block["block_type"].as_i64() {
        Some(2) => "text", Some(3) => "heading1", Some(4) => "heading2",
        Some(5) => "heading3", Some(12) => "bullet", Some(13) => "ordered",
        Some(15) => "quote",
        _ => return Err(problem(StatusCode::BAD_REQUEST, "Unsupported block type")),
    };
    let elements = block[field]["elements"].as_array().ok_or_else(||
        problem(StatusCode::BAD_REQUEST, "Missing text elements"))?;
    if elements.is_empty() || elements.len() > 100 || elements.iter().any(|e|
        e["text_run"]["content"].as_str().is_none() || e.as_object().is_none()
        || e.as_object().is_some_and(|o| o.len() != 1 || !o.contains_key("text_run"))
    ) {
        return Err(problem(StatusCode::BAD_REQUEST, "Mixed or unsupported rich text elements"));
    }
    Ok(elements.clone())
}
fn patch_runs_preserving_styles(existing: &[Value], edits: &[RunEdit]) -> ApiResult<Vec<Value>> {
    if existing.len() != edits.len() || edits.is_empty() {
        return Err(problem(StatusCode::CONFLICT, "Text run count changed; reload before editing"));
    }
    let total: usize = edits.iter().map(|e| e.text.chars().count()).sum();
    if total > 20000 {
        return Err(problem(StatusCode::BAD_REQUEST, "Combined text exceeds 20000 characters"));
    }
    let mut result = Vec::with_capacity(existing.len());
    for (before, after) in existing.iter().zip(edits.iter()) {
        if before["text_run"]["content"].as_str() != Some(after.expected_text.as_str()) {
            return Err(problem(StatusCode::CONFLICT, "Text run changed; reload before editing"));
        }
        let mut element = before.clone();
        element["text_run"]["content"] = json!(after.text);
        result.push(element);
    }
    Ok(result)
}
async fn replace_runs_internal(s: &AppState, id: &str, block_id: &str, edits: &[RunEdit]) -> ApiResult<Value> {
    if !valid_id(block_id) || block_id == id {
        return Err(problem(StatusCode::BAD_REQUEST, "Invalid editable block ID"));
    }
    let block = lark(s, reqwest::Method::GET, &format!("/docx/v1/documents/{id}/blocks/{block_id}"), None).await?;
    let current = safe_run_elements(&block["block"])?;
    let updated = patch_runs_preserving_styles(&current, edits)?;
    lark(s, reqwest::Method::PATCH, &format!("/docx/v1/documents/{id}/blocks/{block_id}"),
        Some(json!({"update_text_elements":{"elements":updated}}))).await
}
async fn replace_runs(State(s): State<AppState>, headers: HeaderMap, Path((id, block_id)): Path<(String, String)>, Json(req): Json<ReplaceRuns>) -> ApiResult<Json<Value>> {
    allowed(&s, &headers, &id, true).await?;
    Ok(Json(replace_runs_internal(&s, &id, &block_id, &req.runs).await?))
}
async fn replace_block(State(s): State<AppState>, headers: HeaderMap, Path((id, block_id)): Path<(String, String)>, Json(req): Json<ReplaceBlock>) -> ApiResult<Json<Value>> {
    allowed(&s, &headers, &id, true).await?;
    Ok(Json(replace_block_internal(&s, &id, &block_id, &req.text, req.expected_text.as_deref()).await?))
}
fn verify_delete_candidate(items: &[Value], index: usize, block_id: &str) -> ApiResult<()> {
    let current = items.get(index).and_then(|block| block["block_id"].as_str());
    if current != Some(block_id) {
        return Err(problem(StatusCode::CONFLICT, "Target block moved or is not present at the expected index"));
    }
    Ok(())
}
async fn delete_block_internal(s: &AppState, id: &str, req: &DeleteBlockRequest) -> ApiResult<Value> {
    if !valid_id(&req.parent_id) || !valid_id(&req.block_id)
        || req.block_id == id || req.block_id == req.parent_id
        || req.document_revision_id < 0
        || req.index >= 500
    {
        return Err(problem(StatusCode::BAD_REQUEST, "Invalid delete parameters; require explicit nonnegative document revision"));
    }
    // Read the exact version supplied by the caller. Never fetch the latest and
    // then delete at an unpinned index: concurrent edits could change the target.
    let path = format!("/docx/v1/documents/{id}/blocks/{}/children?document_revision_id={}&page_size=500",
        req.parent_id, req.document_revision_id);
    let children = lark(s, reqwest::Method::GET, &path, None).await?;
    if children["has_more"].as_bool() == Some(true) {
        return Err(problem(StatusCode::CONFLICT, "Parent has more than one page of children; deletion is disabled"));
    }
    let items = children["items"].as_array().ok_or_else(||
        problem(StatusCode::BAD_GATEWAY, "Feishu did not return a valid child block collection"))?;
    verify_delete_candidate(items, req.index, &req.block_id)?;
    let path = format!("/docx/v1/documents/{id}/blocks/{}/children/batch_delete?document_revision_id={}",
        req.parent_id, req.document_revision_id);
    lark(s, reqwest::Method::DELETE, &path,
        Some(json!({"start_index":req.index,"end_index":req.index + 1}))).await
}
async fn delete_block(State(s): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(req): Json<DeleteBlockRequest>) -> ApiResult<Json<Value>> {
    allowed(&s, &headers, &id, true).await?;
    Ok(Json(delete_block_internal(&s, &id, &req).await?))
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
fn mcp_request_validation(message: &Value) -> Result<Option<Value>, Value> {
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || !message.is_object()
        || message.get("method").and_then(Value::as_str).is_none()
    {
        return Err(mcp_error(message.get("id").cloned().unwrap_or(Value::Null), -32600, "Invalid JSON-RPC 2.0 request"));
    }
    let method = message["method"].as_str().unwrap_or("");
    let id = message.get("id").cloned();
    if id.is_none() {
        if method.starts_with("notifications/") { return Ok(None); }
        return Err(mcp_error(Value::Null, -32600, "JSON-RPC method requires an id"));
    }
    if id.as_ref().is_some_and(|id| !(id.is_string() || id.is_number())) {
        return Err(mcp_error(Value::Null, -32600, "JSON-RPC id must be a string or number"));
    }
    Ok(id)
}
async fn mcp(State(s): State<AppState>, headers: HeaderMap, Json(message): Json<Value>) -> impl IntoResponse {
    // Stateless JSON response transport: no SSE stream or session identifiers.
    // Authenticate the endpoint before advertising capabilities or invoking tools.
    if let Err(error) = require(&headers, &s.config, 1) {
        return error.into_response();
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        // Browser-based cross-site calls have no permitted CORS origin. Reject
        // them explicitly rather than allowing cookie-less cross-origin POSTs.
        let origin = origin.to_str().unwrap_or("");
        let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
        let allowed = origin == format!("https://{host}") || origin == format!("http://{host}");
        if !allowed { return problem(StatusCode::FORBIDDEN, "Cross-origin MCP calls are not allowed").into_response(); }
    }
    let id = match mcp_request_validation(&message) {
        Ok(Some(id)) => id,
        Ok(None) => return StatusCode::ACCEPTED.into_response(),
        Err(error) => return (StatusCode::BAD_REQUEST, Json(error)).into_response(),
    };
    let method = message["method"].as_str().unwrap_or("");
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
            {"name":"list_blocks","description":"List first page of registered document blocks","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}},
            {"name":"append_text","description":"Append text to a registered document","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"text":{"type":"string"}},"required":["id","text"]}},
            {"name":"append_structured","description":"Append validated headings, paragraphs, lists or quotes to a registered document","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"blocks":{"type":"array","minItems":1,"maxItems":50,"items":{"type":"object","properties":{"kind":{"type":"string","enum":["text","heading1","heading2","heading3","bullet","ordered","quote"]},"text":{"type":"string"},"bold":{"type":"boolean"},"italic":{"type":"boolean"}},"required":["kind","text"]}}},"required":["id","blocks"]}},
            {"name":"update_text_block","description":"Replace a single unstyled plain-text block in a registered document; refuses rich or mixed elements","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"block_id":{"type":"string"},"text":{"type":"string"},"expected_text":{"type":"string"}},"required":["id","block_id","text"]}},
            {"name":"delete_block","description":"Delete a verified child block at an explicit Feishu document revision and index; requires writer key","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"parent_id":{"type":"string"},"block_id":{"type":"string"},"index":{"type":"integer","minimum":0},"document_revision_id":{"type":"integer","minimum":0}},"required":["id","parent_id","block_id","index","document_revision_id"]}}
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
                    "list_blocks" => {
                        let doc_id = args["id"].as_str().unwrap_or("");
                        allowed(&s, &headers, doc_id, false).await?;
                        lark(&s, reqwest::Method::GET, &format!("/docx/v1/documents/{doc_id}/blocks?page_size=100"), None).await
                    }
                    "append_text" => {
                        let doc_id = args["id"].as_str().unwrap_or("");
                        allowed(&s, &headers, doc_id, true).await?;
                        append_internal(&s, doc_id, args["text"].as_str().unwrap_or("")).await
                    }
                    "append_structured" => {
                        let doc_id = args["id"].as_str().unwrap_or("");
                        allowed(&s, &headers, doc_id, true).await?;
                        let params: AppendStructured = serde_json::from_value(args.clone()).map_err(|_|
                            problem(StatusCode::BAD_REQUEST, "Invalid append_structured arguments"))?;
                        append_structured_internal(&s, doc_id, &params.blocks).await
                    }
                    "update_text_block" => {
                        let doc_id = args["id"].as_str().unwrap_or("");
                        let block_id = args["block_id"].as_str().unwrap_or("");
                        let text = args["text"].as_str().ok_or_else(|| problem(StatusCode::BAD_REQUEST, "Missing text"))?;
                        allowed(&s, &headers, doc_id, true).await?;
                        replace_block_internal(&s, doc_id, block_id, text, args["expected_text"].as_str()).await
                    }
                    "update_text_runs" => {
                        let doc_id = args["id"].as_str().unwrap_or("");
                        let block_id = args["block_id"].as_str().unwrap_or("");
                        allowed(&s, &headers, doc_id, true).await?;
                        let params: ReplaceRuns = serde_json::from_value(args.clone()).map_err(|_|
                            problem(StatusCode::BAD_REQUEST, "Invalid update_text_runs arguments"))?;
                        replace_runs_internal(&s, doc_id, block_id, &params.runs).await
                    }
                    "delete_block" => {
                        let doc_id = args["id"].as_str().unwrap_or("");
                        allowed(&s, &headers, doc_id, true).await?;
                        let params: DeleteBlockRequest = serde_json::from_value(args.clone()).map_err(|_|
                            problem(StatusCode::BAD_REQUEST, "Invalid delete_block arguments"))?;
                        delete_block_internal(&s, doc_id, &params).await
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
    let mut response = (StatusCode::OK, Json(answer)).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response.headers_mut().insert("mcp-protocol-version", "2025-03-26".parse().unwrap());
    response
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
        read_limit_per_minute: env::var("READ_LIMIT_PER_MINUTE").ok().and_then(|v| v.parse().ok()).unwrap_or(120),
        write_limit_per_minute: env::var("WRITE_LIMIT_PER_MINUTE").ok().and_then(|v| v.parse().ok()).unwrap_or(30),
    };
    assert!(config.admin_key.len() >= 32 && config.write_key.len() >= 32 && config.admin_key != config.write_key, "Use distinct API keys of at least 32 characters");
    assert!(config.read_limit_per_minute > 0 && config.write_limit_per_minute > 0, "Rate limits must be positive");
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
        rate_limits: Arc::new(Mutex::new(BTreeMap::new())),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/api/docs", get(list_docs).post(create_doc))
        .route("/api/docs/{id}", get(get_doc))
        .route("/api/docs/{id}/blocks", get(blocks))
        .route("/api/docs/{id}/metadata", get(get_document_metadata))
        .route("/api/docs/{id}/blocks/{parent_id}/children", get(get_children))
        .route("/api/docs/{id}/append", post(append))
        .route("/api/docs/{id}/append-structured", post(append_structured))
        .route("/api/docs/{id}/blocks/{block_id}", axum::routing::patch(replace_block))
        .route("/api/docs/{id}/blocks/{block_id}/runs", axum::routing::patch(replace_runs))
        .route("/api/docs/{id}/blocks/delete", post(delete_block))
        .route("/api/docs/{id}/access", post(set_access))
        .route("/mcp", post(mcp))
        .layer(middleware::from_fn_with_state(state.clone(), security_gate))
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
    #[test] fn per_role_buckets_reset_after_window() {
        let now = Instant::now();
        let mut bucket = RateBucket { started: now, count: 0 };
        assert!(apply_bucket(&mut bucket, 2, now));
        assert!(apply_bucket(&mut bucket, 2, now));
        assert!(!apply_bucket(&mut bucket, 2, now));
        assert!(apply_bucket(&mut bucket, 2, now + Duration::from_secs(61)));
        assert_eq!(bucket.count, 1);
    }
    #[test] fn multi_run_edit_preserves_styles_and_checks_originals() {
        let original = vec![
            json!({"text_run":{"content":"normal"}}),
            json!({"text_run":{"content":"bold","text_element_style":{"bold":true}}})
        ];
        let edits = vec![
            RunEdit{expected_text:"normal".into(),text:"updated".into()},
            RunEdit{expected_text:"bold".into(),text:"strong".into()}
        ];
        let result = patch_runs_preserving_styles(&original, &edits).unwrap();
        assert_eq!(result[1]["text_run"]["text_element_style"]["bold"], true);
        assert_eq!(result[0]["text_run"]["content"], "updated");
        let stale = vec![RunEdit{expected_text:"wrong".into(),text:"oops".into()}, RunEdit{expected_text:"bold".into(),text:"ok".into()}];
        assert!(patch_runs_preserving_styles(&original, &stale).is_err());
        assert!(safe_run_elements(&json!({"block_type":2,"text":{"elements":[{"mention_user":{"user_id":"x"}}]}})).is_err());
    }
    #[test] fn editable_text_run_preserves_style_and_refuses_mixed_elements() {
        let element = json!({"text_run":{"content":"Hello","text_element_style":{"bold":true}}});
        let block = json!({"block_type":3,"heading1":{"elements":[element.clone()]}});
        assert_eq!(editable_text_run(&block).unwrap(), element);
        assert!(editable_text_run(&json!({"block_type":3,"heading1":{"elements":[element.clone(),element]}})).is_err());
        assert!(editable_text_run(&json!({"block_type":27})).is_err());
    }
    #[test] fn structured_block_encoder() {
        let blocks = vec![StructuredBlock { kind: "heading2".into(), text: "Release notes".into(), bold: Some(true), italic: None }];
        let encoded = encode_structured_blocks(&blocks).unwrap();
        assert_eq!(encoded[0]["block_type"], 4);
        assert_eq!(encoded[0]["heading2"]["elements"][0]["text_run"]["text_element_style"]["bold"], true);
        assert!(encode_structured_blocks(&[StructuredBlock { kind: "image".into(), text: "not supported".into(), bold: None, italic: None }]).is_err());
        assert!(encode_structured_blocks(&[]).is_err());
    }
    #[test] fn block_pagination_token_validation() {
        assert!(block_page_path("docx_abc", None).unwrap().contains("page_size=100"));
        assert!(block_page_path("docx_abc", Some("abc-123_")).unwrap().ends_with("page_token=abc-123_"));
        assert!(block_page_path("docx_abc", Some("abc+/=")).unwrap().ends_with("page_token=abc%2B%2F%3D"));
        assert!(block_page_path("docx_abc", Some("bad\ntoken")).is_err());
        assert!(block_page_path("docx_abc", Some("")).is_err());
    }
    #[test] fn id_validation() {
        assert!(valid_id("docx_abc-123"));
        assert!(!valid_id("../secret"));
        assert!(!valid_id(""));
    }
    #[test] fn jsonrpc_request_validation() {
        assert!(mcp_request_validation(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"})).is_ok());
        assert!(mcp_request_validation(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).unwrap().is_none());
        assert!(mcp_request_validation(&json!({"jsonrpc":"2.0","method":"tools/list"})).is_err());
        assert!(mcp_request_validation(&json!({"jsonrpc":"1.0","id":1,"method":"ping"})).is_err());
        assert!(mcp_request_validation(&json!([{"jsonrpc":"2.0","id":1,"method":"ping"}])).is_err());
        assert!(mcp_request_validation(&json!({"jsonrpc":"2.0","id":null,"method":"ping"})).is_err());
    }
    #[test] fn guarded_delete_rejects_stale_position() {
        let items = vec![json!({"block_id":"alpha"}), json!({"block_id":"beta"})];
        assert!(verify_delete_candidate(&items, 1, "beta").is_ok());
        assert!(verify_delete_candidate(&items, 1, "alpha").is_err());
        assert!(verify_delete_candidate(&items, 2, "beta").is_err());
    }
    #[test] fn timing_safe_equal() {
        assert!(equal("somekey", "somekey"));
        assert!(!equal("somekey", "anotherkey"));
    }
}
