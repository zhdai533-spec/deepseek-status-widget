//! 本地统计：Widget 作为 127.0.0.1 代理记录「经过这里」的 API 调用。
//!
//! 只精确记录 tokens 与模型，不算费用（官方价格时常变动）；
//! 未登录官网 / 没有经过代理的调用，界面显示「未登录」，不编造数据。

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tiny_http::{Header, Response, Server, StatusCode};

pub const PROXY_PORT: u16 = 3666;
const UPSTREAM: &str = "https://api.deepseek.com";
const MAX_BODY: u64 = 64 * 1024 * 1024;

pub fn proxy_url() -> String {
    format!("http://127.0.0.1:{PROXY_PORT}")
}

#[derive(Serialize, Deserialize)]
struct Event {
    date: String,
    ts_ms: u64,
    model: String,
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Serialize, Default, Clone)]
pub struct UsageTotals {
    pub today_tokens: u64,
    pub month_tokens: u64,
    pub current_model: String,
}

#[repr(C)]
struct SystemTimeRaw {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    millis: u16,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLocalTime(lp_system_time: *mut SystemTimeRaw);
    fn GetSystemTime(lp_system_time: *mut SystemTimeRaw);
}

fn read_system_time(is_utc: bool) -> String {
    let mut t = SystemTimeRaw {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        millis: 0,
    };
    unsafe {
        if is_utc {
            GetSystemTime(&mut t)
        } else {
            GetLocalTime(&mut t)
        }
    };
    format!("{:04}-{:02}-{:02}", t.year, t.month, t.day)
}

pub fn local_today() -> String {
    read_system_time(false)
}

/// 平台用量接口按 UTC 切分每日数据，日期桶要用 UTC 今日去匹配。
pub fn utc_today() -> String {
    read_system_time(true)
}

fn usage_file() -> PathBuf {
    let dir = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("DeepSeekStatusWidget");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("usage.jsonl")
}

fn record_event(model: &str, input: u64, output: u64) {
    let event = Event {
        date: local_today(),
        ts_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        model: model.to_string(),
        input_tokens: input,
        output_tokens: output,
    };
    if let Ok(line) = serde_json::to_string(&event) {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(usage_file()) {
            let _ = writeln!(f, "{line}");
        }
    }
}

pub fn totals() -> UsageTotals {
    let today = local_today();
    let month = &today[..7];
    let mut out = UsageTotals::default();
    if let Ok(file) = std::fs::File::open(usage_file()) {
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if let Ok(ev) = serde_json::from_str::<Event>(&line) {
                if ev.date == today {
                    out.today_tokens += ev.input_tokens + ev.output_tokens;
                }
                if ev.date.starts_with(month) {
                    out.month_tokens += ev.input_tokens + ev.output_tokens;
                    if !ev.model.is_empty() {
                        out.current_model = ev.model;
                    }
                }
            }
        }
    }
    out
}

/// 从响应 JSON 中提取 (模型, 输入tokens, 输出tokens)。
/// 同时兼容 OpenAI 格式的 prompt/completion_tokens 与 Anthropic 格式的 input/output_tokens。
fn usage_from_json(value: &serde_json::Value) -> Option<(String, u64, u64)> {
    let usage = &value["usage"];
    let input = usage["prompt_tokens"].as_u64().or_else(|| usage["input_tokens"].as_u64())?;
    let output = usage["completion_tokens"]
        .as_u64()
        .or_else(|| usage["output_tokens"].as_u64())?;
    if input + output == 0 {
        return None;
    }
    let model = value["model"].as_str().unwrap_or("").to_string();
    Some((model, input, output))
}

pub fn start(app: AppHandle) {
    let server = match Server::http(format!("127.0.0.1:{PROXY_PORT}")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("本地统计代理启动失败（可能端口被占用）: {e}");
            return;
        }
    };
    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            let app = app.clone();
            std::thread::spawn(move || handle_request(request, app));
        }
    });
}

fn header_value(headers: &[Header], name: &'static str) -> Option<String> {
    headers
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.to_string())
}

fn response_text(status: u16, content_type: &str, text: String) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = text.into_bytes();
    let ct = Header::from_bytes("Content-Type", content_type).unwrap();
    Response::new(
        StatusCode(status),
        vec![ct],
        std::io::Cursor::new(body.clone()),
        Some(body.len()),
        None,
    )
}

fn handle_request(mut request: tiny_http::Request, app: AppHandle) {
    let url = format!("{UPSTREAM}{}", request.url());
    let method = request.method().as_str().to_string();

    let mut body = Vec::new();
    if request
        .headers()
        .iter()
        .any(|h| h.field.equiv("Content-Length") && h.value.as_str() != "0")
    {
        let _ = request.as_reader().take(MAX_BODY).read_to_end(&mut body);
    }

    let auth = header_value(request.headers(), "Authorization")
        .or_else(crate::deepseek::load_api_key)
        .map(|key| if key.starts_with("Bearer ") { key } else { format!("Bearer {key}") });

    let Some(auth) = auth else {
        let _ = request.respond(response_text(
            401,
            "application/json",
            r#"{"error":"DeepSeek Status Widget: 未配置 API Key"}"#.into(),
        ));
        return;
    };

    let agent = ureq::AgentBuilder::new().build();
    let mut req = agent.request(&method, &url);
    for h in request.headers() {
        let name = h.field.to_string().to_ascii_lowercase();
        if matches!(name.as_str(), "host" | "content-length" | "connection" | "accept-encoding") {
            continue;
        }
        req = req.set(&name, &h.value.to_string());
    }
    req = req.set("Authorization", &auth);

    let upstream = if body.is_empty() {
        req.call()
    } else {
        req.send_bytes(&body)
    };

    let response = match upstream {
        Ok(resp) => resp,
        Err(ureq::Error::Status(status, resp)) => {
            let content_type =
                resp.header("content-type").unwrap_or("text/plain").to_string();
            let text = resp.into_string().unwrap_or_default();
            let _ = request.respond(response_text(status, &content_type, text));
            return;
        }
        Err(ureq::Error::Transport(e)) => {
            let _ = request.respond(response_text(502, "text/plain", format!("网络错误: {e}")));
            return;
        }
    };

    let status = response.status();
    let content_type = response
        .header("content-type")
        .unwrap_or("application/json")
        .to_string();

    if content_type.contains("text/event-stream") {
        let reader = UsageStreamReader::new(response.into_reader(), app);
        let ct = Header::from_bytes("Content-Type", content_type.as_str()).unwrap();
        let _ = request.respond(Response::new(
            StatusCode(status),
            vec![ct],
            reader,
            None,
            None,
        ));
    } else {
        let mut buf = Vec::new();
        if response.into_reader().take(MAX_BODY).read_to_end(&mut buf).is_ok() {
            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&buf) {
                if let Some((model, input, output)) = usage_from_json(&json) {
                    record_event(&model, input, output);
                    let _ = app.emit("usage-updated", ());
                }
            }
        }
        let ct = Header::from_bytes("Content-Type", content_type.as_str()).unwrap();
        let _ = request.respond(Response::new(
            StatusCode(status),
            vec![ct],
            std::io::Cursor::new(buf.clone()),
            Some(buf.len()),
            None,
        ));
    }
}

/// 流式转发：把 SSE 字节原样传给客户端，同时逐行解析 data: JSON 里的 usage
struct UsageStreamReader {
    reader: BufReader<Box<dyn Read + Send + Sync>>,
    line: Vec<u8>,
    pending: Vec<u8>,
    captured: bool,
    app: AppHandle,
}

impl UsageStreamReader {
    fn new(reader: Box<dyn Read + Send + Sync>, app: AppHandle) -> Self {
        Self {
            reader: BufReader::new(reader),
            line: Vec::new(),
            pending: Vec::new(),
            captured: false,
            app,
        }
    }

    fn next_line(&mut self) -> std::io::Result<bool> {
        self.line.clear();
        let n = self.reader.read_until(b'\n', &mut self.line)?;
        Ok(n > 0)
    }

    fn process_line(&mut self) {
        let line = self.line.trim_ascii();
        let Some(rest) = line.strip_prefix(b"data:") else {
            return;
        };
        let rest = rest.trim_ascii();
        if rest.is_empty() || rest == b"[DONE]" {
            return;
        }
        let Ok(json) = serde_json::from_slice::<serde_json::Value>(rest) else {
            return;
        };
        if !self.captured {
            if let Some((model, input, output)) = usage_from_json(&json) {
                record_event(&model, input, output);
                self.captured = true;
                let _ = self.app.emit("usage-updated", ());
            }
        }
    }
}

impl Read for UsageStreamReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if !self.pending.is_empty() {
                let n = self.pending.len().min(out.len());
                out[..n].copy_from_slice(&self.pending[..n]);
                self.pending.drain(..n);
                return Ok(n);
            }
            if !self.next_line()? {
                return Ok(0);
            }
            self.process_line();
            self.pending.extend_from_slice(&self.line);
        }
    }
}
