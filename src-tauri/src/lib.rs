use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{Manager, WebviewWindow};
use serde::{Deserialize, Serialize};

mod codex;
mod deepseek;
mod usage;
use codex::{get_rect, move_window, self_main_hwnd, CodexWindowDetector};

/// Widget 与 Codex 的相对布局（逻辑像素，集中在这里方便以后调）
const WIDGET_LEFT_INSET_L: f64 = 0.0; // 用客户端区域坐标后，左缘天然对齐内容区
const ACCOUNT_AREA_H_L: f64 = 36.0; // 底部账户区高度
const WIDGET_BOTTOM_GAP_L: f64 = 10.0; // Widget 底边与账户区的间距
const POLL_MS: u64 = 5; // 高频率轮询 + 命中即动，延迟基本不可感知
const ANCHOR_CODEX_BOTTOM_LEFT: &str = "codex-bottom-left";

/// 保存的是「相对 Codex 左下锚点」的偏移（逻辑像素），不存绝对屏幕坐标
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct WidgetPrefs {
    anchor: String,
    offset_x: f64,
    offset_y: f64,
    show_balance: bool,
    show_tokens: bool,
    show_today_cost: bool,
    show_month_cost: bool,
    start_visible: bool,
    autostart: bool,
    balance_interval_s: u64,
    usage_interval_min: u64,
}

impl Default for WidgetPrefs {
    fn default() -> Self {
        Self {
            anchor: ANCHOR_CODEX_BOTTOM_LEFT.to_string(),
            offset_x: 0.0,
            offset_y: 0.0,
            show_balance: true,
            show_tokens: true,
            show_today_cost: true,
            show_month_cost: true,
            start_visible: true,
            autostart: true,
            balance_interval_s: 60,
            usage_interval_min: 5,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct UiSettings {
    show_balance: bool,
    show_tokens: bool,
    show_today_cost: bool,
    show_month_cost: bool,
    start_visible: bool,
    autostart: bool,
    balance_interval_s: u64,
    usage_interval_min: u64,
}

impl Default for UiSettings {
    fn default() -> Self {
        let p = WidgetPrefs::default();
        Self {
            show_balance: p.show_balance,
            show_tokens: p.show_tokens,
            show_today_cost: p.show_today_cost,
            show_month_cost: p.show_month_cost,
            start_visible: p.start_visible,
            autostart: p.autostart,
            balance_interval_s: p.balance_interval_s,
            usage_interval_min: p.usage_interval_min,
        }
    }
}

fn prefs_file() -> PathBuf {
    let dir = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("DeepSeekStatusWidget");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("prefs.json")
}

fn load_prefs() -> WidgetPrefs {
    let mut prefs: WidgetPrefs = std::fs::read_to_string(prefs_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if prefs.anchor.is_empty() {
        prefs.anchor = ANCHOR_CODEX_BOTTOM_LEFT.to_string();
    }
    prefs
}

fn save_prefs(prefs: &WidgetPrefs) {
    if let Ok(json) = serde_json::to_string_pretty(prefs) {
        let _ = std::fs::write(prefs_file(), json);
    }
}

const RUN_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_NAME: &str = "DeepSeekStatusWidget";
const HKEY_CURRENT_USER: usize = 0x8000_0001;
const KEY_SET_VALUE: u32 = 0x0002;
const REG_SZ: u32 = 1;

#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(
        hkey: usize,
        subkey: *const u16,
        reserved: u32,
        sam: u32,
        out: *mut usize,
    ) -> i32;
    fn RegSetValueExW(
        hkey: usize,
        name: *const u16,
        reserved: u32,
        ty: u32,
        data: *const u8,
        size: u32,
    ) -> i32;
    fn RegDeleteValueW(hkey: usize, name: *const u16) -> i32;
    fn RegCloseKey(hkey: usize) -> i32;
}

fn wide_utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// 把当前程序写进（或移出）HKCU 登录启动项，登录后它一直在后台待命，Codex 一开就显示。
fn apply_autostart(enabled: bool) -> Result<(), String> {
    let run = wide_utf16(RUN_PATH);
    let mut key: usize = 0;
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, run.as_ptr(), 0, KEY_SET_VALUE, &mut key) } != 0 {
        return Err("无法打开注册表启动项".into());
    }

    let result = if enabled {
        let exe = std::env::current_exe()
            .map(|p| format!("\"{}\"", p.display()))
            .map_err(|e| e.to_string())?;
        let name = wide_utf16(RUN_NAME);
        let value: Vec<u16> = exe.encode_utf16().chain(Some(0)).collect();
        unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr() as *const u8,
                (value.len() * 2) as u32,
            )
        }
    } else {
        let name = wide_utf16(RUN_NAME);
        unsafe { RegDeleteValueW(key, name.as_ptr()) }
    };
    unsafe { RegCloseKey(key) };

    match (result, enabled) {
        (0, _) => Ok(()),
        (_, true) => Err("写入登录启动项失败".into()),
        // 删除不存在的值返回 2，视为已清除
        (_, false) => Ok(()),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .expect("main window missing");
            // 窗口初始隐藏；找到 Codex 后再显示
            let prefs = Arc::new(Mutex::new(load_prefs()));
            if let Err(e) = apply_autostart(prefs.lock().unwrap().autostart) {
                eprintln!("设置登录启动项失败: {e}");
            }
            app.manage(prefs.clone());
            usage::start(app.handle().clone());
            let force_reposition = Arc::new(AtomicBool::new(false));
            app.manage(force_reposition.clone());
            std::thread::spawn(move || follow_codex(window, prefs, force_reposition));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            reset_widget_position,
            api_key_save,
            api_key_clear,
            api_key_configured,
            api_test_connection,
            api_balance_fetch,
            user_token_save,
            user_token_clear,
            user_token_configured,
            official_usage_test,
            official_usage_fetch,
            usage_totals,
            proxy_url,
            open_deepseek_login,
            get_ui_settings,
            set_ui_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 设置页「恢复默认位置」会调用：清空偏移并立即保存
#[tauri::command]
fn reset_widget_position(
    prefs: tauri::State<Arc<Mutex<WidgetPrefs>>>,
    force_reposition: tauri::State<Arc<AtomicBool>>,
) {
    let mut prefs = prefs.lock().unwrap();
    prefs.anchor = ANCHOR_CODEX_BOTTOM_LEFT.to_string();
    prefs.offset_x = 0.0;
    prefs.offset_y = 0.0;
    save_prefs(&prefs);
    force_reposition.store(true, Ordering::Relaxed);
}

#[tauri::command]
fn get_ui_settings(prefs: tauri::State<Arc<Mutex<WidgetPrefs>>>) -> UiSettings {
    let p = prefs.lock().unwrap();
    UiSettings {
        show_balance: p.show_balance,
        show_tokens: p.show_tokens,
        show_today_cost: p.show_today_cost,
        show_month_cost: p.show_month_cost,
        start_visible: p.start_visible,
        autostart: p.autostart,
        balance_interval_s: p.balance_interval_s,
        usage_interval_min: p.usage_interval_min,
    }
}

#[tauri::command]
fn set_ui_settings(
    settings: UiSettings,
    prefs: tauri::State<Arc<Mutex<WidgetPrefs>>>,
) -> Result<(), String> {
    let mut p = prefs.lock().unwrap();
    p.show_balance = settings.show_balance;
    p.show_tokens = settings.show_tokens;
    p.show_today_cost = settings.show_today_cost;
    p.show_month_cost = settings.show_month_cost;
    p.start_visible = settings.start_visible;
    p.autostart = settings.autostart;
    p.balance_interval_s = settings.balance_interval_s.max(5);
    p.usage_interval_min = settings.usage_interval_min.max(1);
    save_prefs(&p);
    apply_autostart(p.autostart)
}

#[tauri::command]
fn api_key_save(key: String) -> Result<(), String> {
    deepseek::save_api_key(&key)
}

#[tauri::command]
fn api_key_clear() -> Result<(), String> {
    deepseek::clear_api_key()
}

#[tauri::command]
fn api_key_configured() -> bool {
    deepseek::load_api_key().is_some()
}

#[tauri::command]
fn api_test_connection() -> Result<deepseek::BalanceInfo, String> {
    let key = deepseek::load_api_key().ok_or("尚未保存 API Key")?;
    deepseek::fetch_balance(&key)
}

#[tauri::command]
fn api_balance_fetch() -> Result<deepseek::BalanceInfo, String> {
    let key = deepseek::load_api_key().ok_or("尚未保存 API Key")?;
    deepseek::fetch_balance(&key)
}

#[tauri::command]
fn user_token_save(token: String) -> Result<(), String> {
    deepseek::save_user_token(&token)
}

#[tauri::command]
fn user_token_clear() -> Result<(), String> {
    deepseek::clear_user_token()
}

#[tauri::command]
fn user_token_configured() -> bool {
    deepseek::load_user_token().is_some()
}

#[tauri::command]
fn official_usage_test(token: String) -> Result<deepseek::OfficialUsage, String> {
    deepseek::fetch_official_usage(&token)
}

#[tauri::command]
fn official_usage_fetch() -> Result<deepseek::OfficialUsage, String> {
    let token = deepseek::load_user_token().ok_or("尚未填写官网 userToken")?;
    deepseek::fetch_official_usage(&token)
}

#[tauri::command]
fn usage_totals() -> usage::UsageTotals {
    usage::totals()
}

#[tauri::command]
fn proxy_url() -> String {
    usage::proxy_url()
}

#[tauri::command]
fn open_deepseek_login() -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg("https://platform.deepseek.com/usage")
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("打开浏览器失败: {e}"))
}

fn follow_codex(
    window: WebviewWindow,
    shared: Arc<Mutex<WidgetPrefs>>,
    force_reposition: Arc<AtomicBool>,
) {
    let mut detector = CodexWindowDetector::new();
    let mut last_codex: Option<codex::WinRect> = None;
    let self_hwnd = self_main_hwnd(std::process::id());
    let l = |v: f64, scale: f64| (v * scale).round() as i32;
    let mut need_save = false;
    let mut last_drag = Instant::now();

    loop {
        if force_reposition.swap(false, Ordering::Relaxed) {
            last_codex = None;
        }
        match detector.find() {
            Some(codex) if !codex.minimized => {
                let scale = window.scale_factor().unwrap_or(1.0);
                let (offset_x_l, offset_y_l) = {
                    let p = shared.lock().unwrap();
                    (p.offset_x, p.offset_y)
                };
                let offset_x = (offset_x_l * scale).round() as i32;
                let offset_y = (offset_y_l * scale).round() as i32;
                let widget_h = self_hwnd
                    .and_then(get_rect)
                    .map(|r| r.bottom - r.top)
                    .unwrap_or(38);

                let base_x = codex.rect.left + l(WIDGET_LEFT_INSET_L, scale);
                let base_y = codex.rect.bottom
                    - l(ACCOUNT_AREA_H_L + WIDGET_BOTTOM_GAP_L, scale)
                    - widget_h;
                let expected_x = base_x + offset_x;
                let expected_y = base_y + offset_y;

                if last_codex != Some(codex.rect) {
                    // Codex 移动/缩放 → 把 Widget 贴到目标位置
                    if let Some(hwnd) = self_hwnd {
                        if let Some(wr) = get_rect(hwnd) {
                            if (wr.left - expected_x).abs() > 1 || (wr.top - expected_y).abs() > 1 {
                                move_window(hwnd, expected_x, expected_y);
                            }
                        }
                    }
                } else if let Some(hwnd) = self_hwnd {
                    // Codex 没动而 Widget 动了 → 用户在拖它，记下相对偏移（不反向抢位）
                    if let Some(wr) = get_rect(hwnd) {
                        if (wr.left - expected_x).abs() > 3 || (wr.top - expected_y).abs() > 3 {
                            let mut p = shared.lock().unwrap();
                            p.anchor = ANCHOR_CODEX_BOTTOM_LEFT.to_string();
                            p.offset_x = (wr.left - base_x) as f64 / scale;
                            p.offset_y = (wr.top - base_y) as f64 / scale;
                            need_save = true;
                            last_drag = Instant::now();
                        }
                    }
                }
                if !window.is_visible().unwrap_or(false) {
                    let _ = window.show();
                }
                last_codex = Some(codex.rect);
            }
            _ => {
                // Codex 不存在 / 最小化 / 关闭 → Widget 隐藏，继续等它回来
                let _ = window.hide();
                last_codex = None;
            }
        }
        // 拖动结束约半秒后落盘一次，避免拖动过程中反复写文件
        if need_save && last_drag.elapsed() >= Duration::from_millis(500) {
            let p = shared.lock().unwrap();
            save_prefs(&p);
            need_save = false;
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}
