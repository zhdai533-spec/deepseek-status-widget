//! Codex 主窗口识别（PHASE 2）。
//!
//! 只读取窗口的存在性 / 客户端区域 / 最小化状态，不注入、不读取任何界面内容。
//! 匹配规则集中在这里，识别失败时改下面 CodexWindowDetector::new 的默认值即可。

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;

type HWND = *mut c_void;
type HANDLE = *mut c_void;
type BOOL = i32;
type DWORD = u32;
type LPARAM = isize;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct WinRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
struct NativeRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

impl From<NativeRect> for WinRect {
    fn from(r: NativeRect) -> Self {
        Self { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
    }
}

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(
        lp_enum_func: Option<unsafe extern "system" fn(HWND, LPARAM) -> BOOL>,
        l_param: LPARAM,
    ) -> BOOL;
    fn GetWindowThreadProcessId(h_wnd: HWND, lpdw_process_id: *mut DWORD) -> DWORD;
    fn IsWindowVisible(h_wnd: HWND) -> BOOL;
    fn IsWindow(h_wnd: HWND) -> BOOL;
    fn IsIconic(h_wnd: HWND) -> BOOL;
    fn GetWindowTextW(h_wnd: HWND, lp_string: *mut u16, n_max_count: i32) -> i32;
    fn GetWindowRect(h_wnd: HWND, lp_rect: *mut NativeRect) -> BOOL;
    fn GetClientRect(h_wnd: HWND, lp_rect: *mut NativeRect) -> BOOL;
    fn ClientToScreen(h_wnd: HWND, lp_point: *mut Point) -> BOOL;
    fn SetWindowPos(
        h_wnd: HWND,
        h_wnd_insert_after: HWND,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        u_flags: u32,
    ) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(dw_desired_access: DWORD, b_inherit_handle: BOOL, dw_process_id: DWORD)
        -> HANDLE;
    fn QueryFullProcessImageNameW(
        h_process: HANDLE,
        dw_flags: DWORD,
        lp_exe_name: *mut u16,
        lpdw_size: *mut DWORD,
    ) -> BOOL;
    fn CloseHandle(h_object: HANDLE) -> BOOL;
}

struct Candidate {
    hwnd: HWND,
    pid: u32,
    title: String,
    rect: WinRect,
}

#[derive(Clone, Debug)]
pub struct DetectedCodex {
    pub rect: WinRect,
    pub minimized: bool,
}

/// 窗口匹配规则：process / title / exe path，任一组命中即可。
#[derive(Default)]
pub struct CodexWindowDetector {
    pub process_name_contains: Vec<String>,
    pub title_contains: Vec<String>,
    pub exe_path_contains: Vec<String>,
    cache: HashMap<u32, String>,
    cached_hwnd: Option<HWND>,
}

impl CodexWindowDetector {
    /// 本机实测：Codex Desktop 装在 WindowsApps 的 OpenAI.Codex 包下。
    /// 若以后识别不到，改这里的规则即可。
    pub fn new() -> Self {
        Self {
            process_name_contains: vec!["chatgpt".into()],
            title_contains: vec!["chatgpt".into()],
            exe_path_contains: vec!["openai.codex".into()],
            cache: HashMap::new(),
            cached_hwnd: None,
        }
    }

    /// 拿到句柄后走「便宜路径」：每次只 IsWindow + 客户端区域 + IsIconic，
    /// 不再重复枚举窗口，因此可以把轮询频率提得很高而不吃 CPU。
    pub fn find(&mut self) -> Option<DetectedCodex> {
        let hwnd = match self.cached_hwnd {
            Some(hwnd) if unsafe { IsWindow(hwnd) != 0 } => hwnd,
            _ => {
                let found = self.enumerate_best()?;
                self.cached_hwnd = Some(found);
                found
            }
        };
        let rect = window_content_rect(hwnd)?;
        let minimized = unsafe { IsIconic(hwnd) != 0 };
        Some(DetectedCodex { rect, minimized })
    }

    fn enumerate_best(&mut self) -> Option<HWND> {
        let mut list: Vec<Candidate> = Vec::new();
        unsafe {
            EnumWindows(Some(collect_window), &mut list as *mut Vec<Candidate> as LPARAM);
        }
        list.retain_mut(|c| {
            let exe = self.exe_path(c.pid);
            self.matches(&exe, &c.title)
        });
        // 同进程可能开多个窗口，取面积最大者作为主窗口
        list.sort_by_key(|c| {
            -((c.rect.right as i64 - c.rect.left as i64)
                * (c.rect.bottom as i64 - c.rect.top as i64))
        });
        list.into_iter().next().map(|c| c.hwnd)
    }

    fn matches(&self, exe_path: &str, title: &str) -> bool {
        let exe = exe_path.to_lowercase();
        let name = Path::new(exe_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        let title = title.to_lowercase();

        let mut any_rule = false;
        let mut hit = false;
        for group in [
            (&self.process_name_contains, &name),
            (&self.title_contains, &title),
            (&self.exe_path_contains, &exe),
        ] {
            if !group.0.is_empty() {
                any_rule = true;
                if group.0.iter().any(|k| group.1.contains(k)) {
                    hit = true;
                }
            }
        }
        hit && any_rule
    }

    fn exe_path(&mut self, pid: u32) -> String {
        if let Some(p) = self.cache.get(&pid) {
            return p.clone();
        }
        let mut path = String::new();
        unsafe {
            let handle = OpenProcess(0x1000 /* PROCESS_QUERY_LIMITED_INFORMATION */, 0, pid);
            if !handle.is_null() {
                let mut size: DWORD = 1024;
                let mut buf = [0u16; 1024];
                if QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size) != 0 {
                    let len = (size as usize).min(buf.len());
                    path = String::from_utf16_lossy(&buf[..len]);
                }
                CloseHandle(handle);
            }
        }
        self.cache.insert(pid, path.clone());
        path
    }
}

/// 读取窗口「客户端区域」的屏幕坐标（剔除不可见外框，与内容渲染区一致）
pub fn window_content_rect(h_wnd: HWND) -> Option<WinRect> {
    unsafe {
        let mut client = NativeRect { left: 0, top: 0, right: 0, bottom: 0 };
        if GetClientRect(h_wnd, &mut client) == 0 {
            return None;
        }
        let mut origin = Point { x: 0, y: 0 };
        if ClientToScreen(h_wnd, &mut origin) == 0 {
            return None;
        }
        Some(WinRect {
            left: origin.x,
            top: origin.y,
            right: origin.x + client.right,
            bottom: origin.y + client.bottom,
        })
    }
}

/// 找到本 Widget（Tauri 主窗口）的原生句柄，用同一套物理坐标直接定位
pub fn self_main_hwnd(pid: u32) -> Option<HWND> {
    unsafe {
        let mut find = SelfFind { pid, found: std::ptr::null_mut() };
        EnumWindows(Some(self_window_probe), &mut find as *mut SelfFind as LPARAM);
        if find.found.is_null() {
            None
        } else {
            Some(find.found)
        }
    }
}

unsafe extern "system" fn self_window_probe(h_wnd: HWND, l_param: LPARAM) -> BOOL {
    let find = unsafe { &mut *(l_param as *mut SelfFind) };
    let mut p: DWORD = 0;
    unsafe { GetWindowThreadProcessId(h_wnd, &mut p) };
    if p == find.pid {
        let mut buf = [0u16; 128];
        let n = unsafe { GetWindowTextW(h_wnd, buf.as_mut_ptr(), 128) };
        let title = String::from_utf16_lossy(&buf[..(n as usize).min(buf.len())]);
        if title == "DeepSeek Status Widget" {
            find.found = h_wnd;
            return 0;
        }
    }
    1
}

#[repr(C)]
struct SelfFind {
    pid: u32,
    found: HWND,
}

/// 读取窗口当前屏幕矩形（Widget 自身没有不可见外框，直接用 GetWindowRect）
pub fn get_rect(h_wnd: HWND) -> Option<WinRect> {
    let mut r = NativeRect { left: 0, top: 0, right: 0, bottom: 0 };
    if unsafe { GetWindowRect(h_wnd, &mut r) } != 0 {
        Some(r.into())
    } else {
        None
    }
}

/// SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER：只挪位置，不改尺寸/层级/焦点
const SWP_MOVE_ONLY: u32 = 0x0001 | 0x0004 | 0x0010 | 0x0200;

pub fn move_window(h_wnd: HWND, x: i32, y: i32) {
    unsafe {
        let _ = SetWindowPos(h_wnd, std::ptr::null_mut(), x, y, 0, 0, SWP_MOVE_ONLY);
    }
}

unsafe extern "system" fn collect_window(hwnd: HWND, l_param: LPARAM) -> BOOL {
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let list = unsafe { &mut *(l_param as *mut Vec<Candidate>) };
    let mut pid: DWORD = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return 1;
    }

    let mut title_buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, title_buf.as_mut_ptr(), 512) };
    let title = if len > 0 {
        String::from_utf16_lossy(&title_buf[..(len as usize).min(title_buf.len())])
    } else {
        String::new()
    };

    let rect = window_content_rect(hwnd).unwrap_or_default();

    list.push(Candidate { hwnd, pid, title, rect });
    1
}
