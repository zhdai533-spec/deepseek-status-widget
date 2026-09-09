//! DeepSeek 官方 API：密钥安全保存（Windows 凭据管理器）+ 网络请求。
//!
//! 只调用官方 API，不保存密码 / Cookie，不爬网页。

use std::ffi::c_void;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const API_TARGET: &str = "DeepSeekStatusWidget/ApiKey";
const TOKEN_TARGET: &str = "DeepSeekStatusWidget/UserToken";
const USER: &str = "DeepSeekStatusWidget";
const CRED_TYPE_GENERIC: u32 = 1;
const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
const BALANCE_URL: &str = "https://api.deepseek.com/user/balance";
const PLATFORM_AMOUNT_URL: &str = "https://platform.deepseek.com/api/v0/usage/amount";
const PLATFORM_COST_URL: &str = "https://platform.deepseek.com/api/v0/usage/cost";

#[repr(C)]
struct FileTime {
    low: u32,
    high: u32,
}

#[repr(C)]
struct CredentialW {
    flags: u32,
    type_: u32,
    target_name: *mut u16,
    comment: *mut u16,
    last_written: FileTime,
    credential_blob_size: u32,
    credential_blob: *mut u8,
    persist: u32,
    attribute_count: u32,
    attributes: *mut c_void,
    target_alias: *mut u16,
    user_name: *mut u16,
}

#[link(name = "advapi32")]
extern "system" {
    fn CredWriteW(credential: *const CredentialW, flags: u32) -> i32;
    fn CredReadW(
        target_name: *const u16,
        type_: u32,
        reserved: u32,
        credential: *mut *mut CredentialW,
    ) -> i32;
    fn CredDeleteW(target_name: *const u16, type_: u32, flags: u32) -> i32;
    fn CredFree(credential: *mut c_void);
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn to_utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

fn from_utf16le(bytes: &[u8]) -> Option<String> {
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

fn write_cred(target: &str, value: &str) -> Result<(), String> {
    let target = wide(target);
    let user = wide(USER);
    let blob = to_utf16le(value);
    let credential = CredentialW {
        flags: 0,
        type_: CRED_TYPE_GENERIC,
        target_name: target.as_ptr() as *mut u16,
        comment: std::ptr::null_mut(),
        last_written: FileTime { low: 0, high: 0 },
        credential_blob_size: blob.len() as u32,
        credential_blob: blob.as_ptr() as *mut u8,
        persist: CRED_PERSIST_LOCAL_MACHINE,
        attribute_count: 0,
        attributes: std::ptr::null_mut(),
        target_alias: std::ptr::null_mut(),
        user_name: user.as_ptr() as *mut u16,
    };
    let ok = unsafe { CredWriteW(&credential, 0) } != 0;
    if ok {
        Ok(())
    } else {
        Err("保存到 Windows 凭据管理器失败".into())
    }
}

fn read_cred(target: &str) -> Option<String> {
    let target = wide(target);
    let mut out: *mut CredentialW = std::ptr::null_mut();
    let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut out) } != 0;
    if !ok || out.is_null() {
        return None;
    }
    let cred = unsafe { &*out };
    let bytes = if cred.credential_blob.is_null() {
        Vec::new()
    } else {
        unsafe {
            std::slice::from_raw_parts(cred.credential_blob, cred.credential_blob_size as usize)
                .to_vec()
        }
    };
    unsafe { CredFree(out as *mut c_void) };
    from_utf16le(&bytes)
}

fn delete_cred(target: &str) -> Result<(), String> {
    let target = wide(target);
    if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 {
        Err("清除失败".into())
    } else {
        Ok(())
    }
}

pub fn save_api_key(key: &str) -> Result<(), String> {
    write_cred(API_TARGET, key)
}

pub fn load_api_key() -> Option<String> {
    read_cred(API_TARGET)
}

pub fn clear_api_key() -> Result<(), String> {
    delete_cred(API_TARGET)
}

pub fn save_user_token(token: &str) -> Result<(), String> {
    write_cred(TOKEN_TARGET, token)
}

pub fn load_user_token() -> Option<String> {
    read_cred(TOKEN_TARGET)
}

pub fn clear_user_token() -> Result<(), String> {
    delete_cred(TOKEN_TARGET)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BalanceInfo {
    pub is_available: bool,
    pub currency: String,
    pub total_balance: String,
    pub granted_balance: String,
    pub topped_up_balance: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OfficialUsage {
    pub currency: String,
    pub today_tokens: u64,
    pub today_cost: f64,
    pub month_tokens: u64,
    pub month_cost: f64,
}

impl Default for OfficialUsage {
    fn default() -> Self {
        Self {
            currency: "CNY".into(),
            today_tokens: 0,
            today_cost: 0.0,
            month_tokens: 0,
            month_cost: 0.0,
        }
    }
}

/// 用官网登录态 userToken 拉取「本月/今日」用量与花费。
///
/// 接口是 platform.deepseek.com 的私有接口，可能随时变更；错误码 40002/40003
/// 或 HTTP 401/403 都表示登录态过期，需要用户重新粘贴 userToken。
pub fn fetch_official_usage(user_token: &str) -> Result<OfficialUsage, String> {
    let token = normalize_user_token(user_token);
    if token.is_empty() {
        return Err("userToken 为空".into());
    }
    let today = crate::usage::local_today();
    let year: u32 = today[..4].parse().unwrap_or(0);
    let month: u32 = today[5..7].parse().unwrap_or(0);
    let amount_url = format!("{PLATFORM_AMOUNT_URL}?month={month}&year={year}");
    let cost_url = format!("{PLATFORM_COST_URL}?month={month}&year={year}");

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build();
    let amount = platform_get(&agent, &amount_url, &token)?;
    let cost = platform_get(&agent, &cost_url, &token)?;
    parse_official_usage(&amount, &cost, &crate::usage::utc_today())
}

/// 平台新版可能把 userToken 存成 `{"value":"...","__version":"0"}`，
/// 保存时自动取出 value；也兼容直接粘贴裸 token。
fn normalize_user_token(raw: &str) -> String {
    let raw = raw.trim().trim_matches('"').trim_matches('\'');
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(value) = json["value"].as_str() {
            return value.trim().to_string();
        }
        if let Some(value) = json.as_str() {
            return value.trim().to_string();
        }
    }
    raw.to_string()
}

fn platform_get(
    agent: &ureq::Agent,
    url: &str,
    token: &str,
) -> Result<serde_json::Value, String> {
    agent
        .get(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .set("Referer", "https://platform.deepseek.com/usage")
        .set(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120.0 Safari/537.36",
        )
        .call()
        .map_err(|err| match err {
            ureq::Error::Status(401 | 403, _) => {
                "登录已过期，请重新复制 userToken".into()
            }
            ureq::Error::Status(code, resp) => format!(
                "HTTP {code}: {}",
                resp.into_string().unwrap_or_default().trim()
            ),
            ureq::Error::Transport(t) => format!("网络错误: {t}"),
        })?
        .into_json::<serde_json::Value>()
        .map_err(|e| format!("返回数据解析失败: {e}"))
}

fn code_value(v: &serde_json::Value) -> i64 {
    (*v)["code"].as_i64().unwrap_or(0)
}

fn biz_code_value(v: &serde_json::Value) -> i64 {
    (*v)["data"]["biz_code"].as_i64().unwrap_or(0)
}

fn is_auth_code(code: i64) -> bool {
    code == 40002 || code == 40003
}

fn parse_num(v: &serde_json::Value) -> f64 {
    v.as_str()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .or_else(|| v.as_f64())
        .unwrap_or(0.0)
}

/// 汇总单个模型用量数组里除 REQUEST 外的数量。
fn sum_models(models: &serde_json::Value) -> f64 {
    let mut total = 0.0;
    if let Some(list) = models.as_array() {
        for model in list {
            if let Some(items) = (*model)["usage"].as_array() {
                for item in items {
                    if (*item)["type"].as_str().unwrap_or("") != "REQUEST" {
                        total += parse_num(&(*item)["amount"]);
                    }
                }
            }
        }
    }
    total
}

/// 在 days 数组里找到与 today 相同日期的桶，汇总用量/花费。
fn sum_days(days: &serde_json::Value, today: &str) -> f64 {
    let mut total = 0.0;
    if let Some(list) = days.as_array() {
        for day in list {
            if (*day)["date"].as_str() == Some(today) {
                total += sum_models(&(*day)["data"]);
            }
        }
    }
    total
}

fn parse_official_usage(
    amount: &serde_json::Value,
    cost: &serde_json::Value,
    today: &str,
) -> Result<OfficialUsage, String> {
    for (name, payload) in [("用量", amount), ("花费", cost)] {
        let code = code_value(payload);
        let biz_code = biz_code_value(payload);
        if is_auth_code(code) || is_auth_code(biz_code) {
            return Err("登录已过期，请重新复制 userToken".into());
        }
        if code != 0 || biz_code != 0 {
            let msg = (*payload)["msg"].as_str().unwrap_or("");
            let biz_msg = (*payload)["data"]["biz_msg"].as_str().unwrap_or("");
            return Err(format!(
                "{name}接口返回异常（code={code}, biz_code={biz_code}, {msg} {biz_msg}）"
            ));
        }
    }

    let amount_biz = &(*amount)["data"]["biz_data"];
    let cost_biz = &(*cost)["data"]["biz_data"][0];
    let mut out = OfficialUsage {
        currency: (*cost_biz)["currency"]
            .as_str()
            .unwrap_or("CNY")
            .to_string(),
        ..Default::default()
    };

    out.month_tokens = sum_models(&(*amount_biz)["total"]).round() as u64;
    out.month_cost = sum_models(&(*cost_biz)["total"]);
    out.today_tokens = sum_days(&(*amount_biz)["days"], today).round() as u64;
    out.today_cost = sum_days(&(*cost_biz)["days"], today);
    Ok(out)
}

/// 调用官方 GET /user/balance 验证 Key 是否有效（余额展示在 PHASE 5 使用）
pub fn fetch_balance(api_key: &str) -> Result<BalanceInfo, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(12))
        .build();
    let response = agent
        .get(BALANCE_URL)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Accept", "application/json")
        .call()
        .map_err(|err| match err {
            ureq::Error::Status(401, _) => "API Key 无效（401）".into(),
            ureq::Error::Status(code, resp) => format!(
                "HTTP {code}: {}",
                resp.into_string().unwrap_or_default().trim()
            ),
            ureq::Error::Transport(t) => format!("网络错误: {t}"),
        })?;

    let json: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("返回数据解析失败: {e}"))?;
    let is_available = json["is_available"].as_bool().unwrap_or(false);
    let currency = json["balance_infos"][0]["currency"]
        .as_str()
        .unwrap_or("CNY")
        .to_string();
    let total_balance = json["balance_infos"][0]["total_balance"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let granted_balance = json["balance_infos"][0]["granted_balance"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let topped_up_balance = json["balance_infos"][0]["topped_up_balance"]
        .as_str()
        .unwrap_or("")
        .to_string();
    Ok(BalanceInfo {
        is_available,
        currency,
        total_balance,
        granted_balance,
        topped_up_balance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AMOUNT: &str = r#"{
      "code": 0,
      "data": {
        "biz_code": 0,
        "biz_data": {
          "total": [{
            "model": "deepseek-chat",
            "usage": [
              {"type": "PROMPT_CACHE_HIT_TOKEN", "amount": "100686720"},
              {"type": "PROMPT_CACHE_MISS_TOKEN", "amount": "1305432"},
              {"type": "RESPONSE_TOKEN", "amount": "656338"},
              {"type": "REQUEST", "amount": "1212"}
            ]
          }],
          "days": [{
            "date": "2026-05-26",
            "data": [{
              "model": "deepseek-chat",
              "usage": [
                {"type": "PROMPT_CACHE_HIT_TOKEN", "amount": "100686720"},
                {"type": "PROMPT_CACHE_MISS_TOKEN", "amount": "1305432"},
                {"type": "RESPONSE_TOKEN", "amount": "656338"},
                {"type": "REQUEST", "amount": "1212"}
              ]
            }]
          }]
        }
      }
    }"#;

    const COST: &str = r#"{
      "code": 0,
      "data": {
        "biz_code": 0,
        "biz_data": [{
          "total": [{
            "model": "deepseek-chat",
            "usage": [
              {"type": "PROMPT_CACHE_HIT_TOKEN", "amount": "2.0137344000000000"},
              {"type": "PROMPT_CACHE_MISS_TOKEN", "amount": "1.3054320000000000"},
              {"type": "RESPONSE_TOKEN", "amount": "1.3126760000000000"},
              {"type": "REQUEST", "amount": "0"}
            ]
          }],
          "days": [{
            "date": "2026-05-26",
            "data": [{
              "model": "deepseek-chat",
              "usage": [
                {"type": "PROMPT_CACHE_HIT_TOKEN", "amount": "2.0137344000000000"},
                {"type": "PROMPT_CACHE_MISS_TOKEN", "amount": "1.3054320000000000"},
                {"type": "RESPONSE_TOKEN", "amount": "1.3126760000000000"},
                {"type": "REQUEST", "amount": "0"}
              ]
            }]
          }],
          "currency": "CNY"
        }]
      }
    }"#;

    #[test]
    fn parses_month_and_today_usage() {
        let amount: serde_json::Value = serde_json::from_str(AMOUNT).unwrap();
        let cost: serde_json::Value = serde_json::from_str(COST).unwrap();
        let out = parse_official_usage(&amount, &cost, "2026-05-26").unwrap();
        assert_eq!(out.month_tokens, 102_648_490);
        assert_eq!(out.today_tokens, 102_648_490);
        assert!((out.month_cost - 4.6318424).abs() < 1e-9);
        assert!((out.today_cost - 4.6318424).abs() < 1e-9);
        assert_eq!(out.currency, "CNY");
    }

    #[test]
    fn wrong_today_bucket_is_zero() {
        let amount: serde_json::Value = serde_json::from_str(AMOUNT).unwrap();
        let cost: serde_json::Value = serde_json::from_str(COST).unwrap();
        let out = parse_official_usage(&amount, &cost, "2026-05-27").unwrap();
        assert_eq!(out.today_tokens, 0);
        assert_eq!(out.today_cost, 0.0);
    }

    #[test]
    fn expired_token_error() {
        let mut amount: serde_json::Value = serde_json::from_str(AMOUNT).unwrap();
        amount["code"] = serde_json::json!(40002);
        let cost: serde_json::Value = serde_json::from_str(COST).unwrap();
        let err = parse_official_usage(&amount, &cost, "2026-05-26").unwrap_err();
        assert!(err.contains("已过期"));
    }

    #[test]
    fn unwraps_wrapped_token() {
        assert_eq!(
            normalize_user_token(
                r#"{"value":"sess-test-token-1234567890abcdef","__version":"0"}"#
            ),
            "sess-test-token-1234567890abcdef"
        );
        assert_eq!(normalize_user_token("abc-123"), "abc-123");
    }
}
