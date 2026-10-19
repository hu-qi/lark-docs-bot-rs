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

    let auth_response = client.post(format!("{base}/auth/v3/tenant_access_token/internal"))
        .json(&json!({"app_id":app_id,"app_secret":app_secret}))
        .send().await.map_err(|e| format!("Authentication request failed: {e}"))?;
    let auth_status = auth_response.status();
    let auth: Value = auth_response.json().await.map_err(|e| format!("Authentication response malformed: {e}"))?;
    if !auth_status.is_success() || auth["code"].as_i64() != Some(0) {
        return Err(format!("Authentication failed: HTTP {} code={}", auth_status, auth["code"]));
    }
    let token = auth["tenant_access_token"].as_str().ok_or("Missing tenant token")?;
    println!("STEP 1/5: tenant_access_token granted");

    // Running this creates a new document in the application's default location.
    let marker = format!("Lark Docs Bot API Smoke {}", env::var("GITHUB_RUN_ID").unwrap_or_else(|_| "manual".into()));
    let created = call(&client, Method::POST, &format!("{base}/docx/v1/documents"), Some(token),
        Some(json!({"title":marker}))).await?;
    let id = created["document"]["document_id"].as_str().ok_or("Missing document_id")?;
    println!("STEP 2/5: create document OK (id: {id})");
    println!("Test document URL: https://www.feishu.cn/docx/{id}");

    let text = "Automated integration test: 飞书文档读写验证";
    let inserted = call(&client, Method::POST,
        &format!("{base}/docx/v1/documents/{id}/blocks/{id}/children"), Some(token),
        Some(json!({"children":[{"block_type":2,"text":{"elements":[{"text_run":{"content":text}}]}}]}))).await?;
    let block_id = inserted["children"][0]["block_id"].as_str().ok_or("Missing created block_id")?;
    println!("STEP 3/5: append text block OK");

    let read = call(&client, Method::GET, &format!("{base}/docx/v1/documents/{id}/raw_content"), Some(token), None).await?;
    let content = read["content"].as_str().ok_or("Missing raw_content")?;
    if !content.contains(text) { return Err("Readback did not contain expected text".into()); }
    println!("STEP 4/5: readback content verified");

    let replacement = "Automated integration test: 修改段落成功";
    call(&client, Method::PATCH,
        &format!("{base}/docx/v1/documents/{id}/blocks/{block_id}"), Some(token),
        Some(json!({"update_text_elements":{"elements":[{"text_run":{"content":replacement}}]}}))).await?;
    let updated = call(&client, Method::GET,
        &format!("{base}/docx/v1/documents/{id}/raw_content"), Some(token), None).await?;
    if !updated["content"].as_str().unwrap_or("").contains(replacement) {
        return Err("Updated block did not round-trip".into());
    }
    println!("STEP 5/5: block update and readback verified");
    // This optional destructive check is restricted to a document created
    // during THIS test run. It never accepts an external document identifier.
    if env::var("FEISHU_TEST_DELETE").as_deref() == Ok("true") {
        let metadata = call(&client, Method::GET,
            &format!("{base}/docx/v1/documents/{id}"), Some(token), None).await?;
        let revision = metadata["document"]["revision_id"].as_i64()
            .ok_or("Document metadata did not return numeric revision_id")?;
        if revision < 0 { return Err("Invalid revision".into()); }
        let children = call(&client, Method::GET,
            &format!("{base}/docx/v1/documents/{id}/blocks/{id}/children?document_revision_id={revision}&page_size=500"),
            Some(token), None).await?;
        if children["has_more"].as_bool() == Some(true) {
            return Err("Incomplete children page; refusing deletion".into());
        }
        let blocks = children["items"].as_array().ok_or("Missing child block list")?;
        let matches: Vec<usize> = blocks.iter().enumerate()
            .filter_map(|(i, item)| (item["block_id"].as_str() == Some(block_id)).then_some(i)).collect();
        if matches.len() != 1 { return Err("Target block missing or duplicated; refusing deletion".into()); }
        let index = matches[0];
        if blocks[index]["block_type"].as_i64() != Some(2) {
            return Err("Target is not a text block".into());
        }
        call(&client, Method::DELETE,
            &format!("{base}/docx/v1/documents/{id}/blocks/{id}/children/batch_delete?document_revision_id={revision}"),
            Some(token), Some(json!({"start_index":index,"end_index":index+1}))).await?;
        let after = call(&client, Method::GET,
            &format!("{base}/docx/v1/documents/{id}/raw_content"), Some(token), None).await?;
        if after["content"].as_str().unwrap_or("").contains(replacement) {
            return Err("Deleted text still appears in document".into());
        }
        println!("STEP 6/6: revision-pinned deletion verified in newly created test document");
    }
    if env::var("FEISHU_TEST_STRUCTURED").as_deref() == Ok("true") {
        let structured = call(&client, Method::POST,
            &format!("{base}/docx/v1/documents/{id}/blocks/{id}/children"), Some(token),
            Some(json!({"children":[
                {"block_type":3,"heading1":{"elements":[{"text_run":{"content":"Smoke Heading"}}]}},
                {"block_type":12,"bullet":{"elements":[{"text_run":{"content":"Smoke bullet item"}}]}},
                {"block_type":13,"ordered":{"elements":[{"text_run":{"content":"Smoke ordered item"}}]}},
                {"block_type":15,"quote":{"elements":[{"text_run":{"content":"Smoke quote"}}]}}
            ]}))).await?;
        if structured["children"].as_array().map(Vec::len) != Some(4) {
            return Err("Structured block creation returned unexpected block count".into());
        }
        let read = call(&client, Method::GET,
            &format!("{base}/docx/v1/documents/{id}/raw_content"), Some(token), None).await?;
        let content = read["content"].as_str().ok_or("Missing raw text after structured insert")?;
        for expected in ["Smoke Heading","Smoke bullet item","Smoke ordered item","Smoke quote"] {
            if !content.contains(expected) { return Err("Structured block readback mismatch".into()); }
        }
        let heading_id = structured["children"][0]["block_id"].as_str()
            .ok_or("Missing structured heading block_id")?;
        let updated_heading = "Smoke Heading Updated";
        call(&client, Method::PATCH,
            &format!("{base}/docx/v1/documents/{id}/blocks/{heading_id}"), Some(token),
            Some(json!({"update_text_elements":{"elements":[{"text_run":{"content":updated_heading}}]}}))).await?;
        let after_update = call(&client, Method::GET,
            &format!("{base}/docx/v1/documents/{id}/raw_content"), Some(token), None).await?;
        if !after_update["content"].as_str().unwrap_or("").contains(updated_heading) {
            return Err("Structured heading update did not round-trip".into());
        }
        println!("Structured heading/list/quote insert, heading edit and readback verified");
    }
    if env::var("FEISHU_TEST_MULTIRUN").as_deref() == Ok("true") {
        let multi = call(&client, Method::POST,
            &format!("{base}/docx/v1/documents/{id}/blocks/{id}/children"), Some(token),
            Some(json!({"children":[{"block_type":2,"text":{"elements":[
                {"text_run":{"content":"Plain "}},
                {"text_run":{"content":"bold","text_element_style":{"bold":true}}}
            ]}}]}))).await?;
        let run_id = multi["children"][0]["block_id"].as_str()
            .ok_or("Missing multi-run block ID")?;
        let before = call(&client, Method::GET,
            &format!("{base}/docx/v1/documents/{id}/blocks/{run_id}"), Some(token), None).await?;
        let elements = before["block"]["text"]["elements"].as_array()
            .ok_or("Missing multi-run text elements")?;
        if elements.len() != 2 || elements[0]["text_run"]["content"] != "Plain "
            || elements[1]["text_run"]["content"] != "bold" {
            return Err("Feishu did not preserve the two initial text runs".into());
        }
        let mut edited = elements.clone();
        edited[0]["text_run"]["content"] = json!("Updated ");
        edited[1]["text_run"]["content"] = json!("strong");
        edited[0]["text_run"]["text_element_style"] = json!({"bold":true});
        edited[1]["text_run"]["text_element_style"]["italic"] = json!(true);
        call(&client, Method::PATCH,
            &format!("{base}/docx/v1/documents/{id}/blocks/{run_id}"), Some(token),
            Some(json!({"update_text_elements":{"elements":edited}}))).await?;
        let after = call(&client, Method::GET,
            &format!("{base}/docx/v1/documents/{id}/blocks/{run_id}"), Some(token), None).await?;
        let updated = after["block"]["text"]["elements"].as_array()
            .ok_or("Multi-run readback missing elements")?;
        if updated.len() != 2
            || updated[0]["text_run"]["content"] != "Updated "
            || updated[1]["text_run"]["content"] != "strong"
            || updated[0]["text_run"]["text_element_style"]["bold"] != true
            || updated[1]["text_run"]["text_element_style"]["bold"] != true
            || updated[1]["text_run"]["text_element_style"]["italic"] != true {
            return Err("Multi-run formatting or content did not round-trip".into());
        }
        println!("Multi-run content and explicit bold/italic formatting verified on disposable document");
    }
    println!("Smoke test succeeded. Document and block IDs: {id} / {block_id}");
    Ok(())
}
