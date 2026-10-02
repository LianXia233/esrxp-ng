//! HTTP 客户端：本地后端调用（阻塞式，工作线程内使用）。
//! 原生 GUI 请求不带浏览器 Origin，不触发后端 CORS 限制。

use serde_json::Value;

pub fn get_json(base: &str, path: &str) -> Result<Value, String> {
    let resp = reqwest::blocking::get(format!("{base}{path}")).map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let text = resp.text().unwrap_or_default();
        return Err(extract_error(&text));
    }
    resp.json::<Value>().map_err(|e| e.to_string())
}

pub fn post_json(base: &str, path: &str, body: &Value) -> Result<Value, String> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{base}{path}"))
        .json(body)
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let text = resp.text().unwrap_or_default();
        return Err(extract_error(&text));
    }
    resp.json::<Value>().map_err(|e| e.to_string())
}

pub fn get_bytes(base: &str, path: &str) -> Result<Vec<u8>, String> {
    let resp = reqwest::blocking::get(format!("{base}{path}")).map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let text = resp.text().unwrap_or_default();
        return Err(extract_error(&text));
    }
    resp.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())
}

fn extract_error(text: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
            return e.to_string();
        }
    }
    text.chars().take(200).collect()
}
