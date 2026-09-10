//! 本机日期辅助 + 历史本地用量（只读）。
//!
//! 早前版本自带一个 127.0.0.1 转发代理来记录模型/用量，它一旦出问题就成了
//! Codex 访问 DeepSeek 的单点故障（挂掉即 502），现已移除；余额与账单直连官方。
//! 这里只保留历史 usage.jsonl 的读取，作为「今日/本月 Tokens」在未登录时的兜底。

use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Event {
    date: String,
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Serialize, Default, Clone)]
pub struct UsageTotals {
    pub today_tokens: u64,
    pub month_tokens: u64,
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
    dir.join("usage.jsonl")
}

pub fn totals() -> UsageTotals {
    let today = local_today();
    let month = &today[..7];
    let mut out = UsageTotals::default();
    if let Ok(file) = std::fs::File::open(usage_file()) {
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if let Ok(ev) = serde_json::from_str::<Event>(&line) {
                let tokens = ev.input_tokens + ev.output_tokens;
                if ev.date == today {
                    out.today_tokens += tokens;
                }
                if ev.date.starts_with(month) {
                    out.month_tokens += tokens;
                }
            }
        }
    }
    out
}
