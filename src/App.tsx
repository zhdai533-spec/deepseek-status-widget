import { useEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  getCurrentWindow,
  LogicalPosition,
  LogicalSize,
} from "@tauri-apps/api/window";
import "./App.css";

const win = getCurrentWindow();
const WIDTH = 275;
const H_COLLAPSED = 38;
const DRAG_THRESHOLD_PX = 5;
const ANIM_MS = 200;
const APP_VERSION = "0.1.0";

type View = "stats" | "settings";

interface ApiMsg {
  ok: boolean;
  text: string;
}

interface Balance {
  is_available: boolean;
  currency: string;
  total_balance: string;
  granted_balance: string;
  topped_up_balance: string;
}

interface UsageTotals {
  today_tokens: number;
  month_tokens: number;
  current_model: string;
}

interface OfficialUsage {
  currency: string;
  today_tokens: number;
  today_cost: number;
  month_tokens: number;
  month_cost: number;
}

interface UiSettings {
  show_balance: boolean;
  show_tokens: boolean;
  show_today_cost: boolean;
  show_month_cost: boolean;
  start_visible: boolean;
  balance_interval_s: number;
  usage_interval_min: number;
}

const DEFAULT_UI: UiSettings = {
  show_balance: true,
  show_tokens: true,
  show_today_cost: true,
  show_month_cost: true,
  start_visible: true,
  balance_interval_s: 60,
  usage_interval_min: 5,
};

function fmtMoney(b: Balance) {
  if (b.currency === "CNY") return `¥${b.total_balance || "—"}`;
  return `${b.currency} ${b.total_balance || "—"}`;
}

function fmtTokens(n: number) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return n > 0 ? String(n) : "";
}

function fmtTokens0(n: number) {
  return n > 0 ? fmtTokens(n) : "0";
}

function fmtCost(v: number, currency: string) {
  const s = (v >= 1 ? v.toFixed(2) : v.toFixed(4))
    .replace(/0+$/, "")
    .replace(/\.$/, "");
  return currency === "CNY" ? `¥${s}` : `${currency} ${s}`;
}

function easeOutCubic(t: number) {
  return 1 - Math.pow(1 - t, 3);
}

function Metric({
  label,
  value,
  sub,
}: {
  label: string;
  value: string;
  sub?: string;
}) {
  return (
    <div className="metric">
      <span className="label">{label}</span>
      <span className="value">{value}</span>
      {sub && <span className="value-sub">{sub}</span>}
    </div>
  );
}

function ToggleRow({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: () => void;
}) {
  return (
    <label className="toggle-row">
      <input type="checkbox" checked={checked} onChange={onChange} />
      <span>{label}</span>
    </label>
  );
}

function App() {
  const panelRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ sx: number; sy: number; started: boolean } | null>(null);
  const busyRef = useRef(false);
  const openRef = useRef(false);
  const heightRef = useRef(H_COLLAPSED);
  const [open, setOpen] = useState(false);
  const [view, setView] = useState<View>("stats");
  const [configured, setConfigured] = useState(false);
  const [key, setKey] = useState("");
  const [testing, setTesting] = useState(false);
  const [apiMsg, setApiMsg] = useState<ApiMsg | null>(null);
  const [token, setToken] = useState("");
  const [tokenBusy, setTokenBusy] = useState(false);
  const [tokenConfigured, setTokenConfigured] = useState(false);
  const [official, setOfficial] = useState<OfficialUsage | null>(null);
  const [officialMsg, setOfficialMsg] = useState<ApiMsg | null>(null);
  const [copiedCmd, setCopiedCmd] = useState(false);
  const [balance, setBalance] = useState<Balance | null>(null);
  const [usage, setUsage] = useState<UsageTotals>({
    today_tokens: 0,
    month_tokens: 0,
    current_model: "",
  });
  const [proxyUrl, setProxyUrl] = useState("");
  const [ui, setUi] = useState<UiSettings>(DEFAULT_UI);
  const [posMsg, setPosMsg] = useState<string | null>(null);
  const [status, setStatus] = useState<"idle" | "updating" | "ok" | "err">(
    "idle",
  );

  useEffect(() => {
    invoke<boolean>("api_key_configured").then(setConfigured).catch(() => {});
    invoke<boolean>("user_token_configured")
      .then(setTokenConfigured)
      .catch(() => {});
    invoke<string>("proxy_url").then(setProxyUrl).catch(() => {});
    invoke<UiSettings>("get_ui_settings").then(setUi).catch(() => {});
  }, []);

  function saveUi(patch: Partial<UiSettings>) {
    setUi((prev) => {
      const next = { ...prev, ...patch };
      void invoke("set_ui_settings", { settings: next }).catch(() => {});
      return next;
    });
  }

  async function refreshTotals() {
    try {
      setUsage(await invoke<UsageTotals>("usage_totals"));
    } catch {
      // 本地统计不可用时保持现状
    }
  }

  async function refreshOfficial() {
    if (!tokenConfigured) return;
    try {
      setOfficial(await invoke<OfficialUsage>("official_usage_fetch"));
      setOfficialMsg(null);
    } catch (err) {
      // 保留上次成功的数据；过期/失败在设置页里提示
      setOfficialMsg({ ok: false, text: String(err) });
    }
  }

  async function refreshAll() {
    await Promise.all([
      refreshBalance(),
      refreshOfficial(),
      refreshTotals(),
    ]);
  }

  useEffect(() => {
    void refreshTotals();
    if (tokenConfigured) void refreshOfficial();
    const unlistenPromise = listen("usage-updated", () => void refreshTotals());
    const timer = window.setInterval(
      () => {
        void refreshTotals();
        if (tokenConfigured) void refreshOfficial();
      },
      ui.usage_interval_min * 60_000,
    );
    return () => {
      window.clearInterval(timer);
      void unlistenPromise.then((unlisten) => unlisten());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tokenConfigured, ui.usage_interval_min]);

  async function refreshBalance() {
    if (!configured) return;
    setStatus("updating");
    try {
      const info = await invoke<Balance>("api_balance_fetch");
      setBalance(info);
      setStatus("ok");
      void refreshTotals();
    } catch {
      // 保留上一次成功数据，只改状态
      setStatus("err");
    }
  }

  useEffect(() => {
    if (!configured) return;
    void refreshBalance();
    const timer = window.setInterval(
      () => void refreshBalance(),
      ui.balance_interval_s * 1000,
    );
    return () => window.clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [configured, ui.balance_interval_s]);

  async function currentLogicalPos() {
    const p = await win.outerPosition();
    const scale = window.devicePixelRatio || 1;
    return { x: p.x / scale, y: p.y / scale };
  }

  async function animateToHeight(toH: number, bottom: number, x: number) {
    const fromH = heightRef.current;
    const start = performance.now();
    return new Promise<void>((resolve) => {
      const step = async () => {
        const t = Math.min(1, (performance.now() - start) / ANIM_MS);
        const h = fromH + (toH - fromH) * easeOutCubic(t);
        await win.setSize(new LogicalSize(WIDTH, Math.max(1, Math.round(h))));
        // 固定窗口底边，向上展开
        await win.setPosition(new LogicalPosition(x, Math.round(bottom - h)));
        if (t < 1) requestAnimationFrame(step);
        else resolve();
      };
      requestAnimationFrame(step);
    });
  }

  async function toggle() {
    if (busyRef.current) return;
    busyRef.current = true;
    const pos = await currentLogicalPos();
    const bottom = pos.y + heightRef.current;

    try {
      if (!openRef.current) {
        flushSync(() => setOpen(true));
        const panelH = panelRef.current?.offsetHeight ?? 180;
        const targetH = H_COLLAPSED + panelH;
        await animateToHeight(targetH, bottom, pos.x);
        heightRef.current = targetH;
        openRef.current = true;
      } else {
        await animateToHeight(H_COLLAPSED, bottom, pos.x);
        heightRef.current = H_COLLAPSED;
        flushSync(() => setOpen(false));
        openRef.current = false;
      }
    } finally {
      busyRef.current = false;
    }
  }

  async function switchView(next: View) {
    if (busyRef.current || next === view) return;
    busyRef.current = true;
    try {
      flushSync(() => setView(next));
      const pos = await currentLogicalPos();
      const bottom = pos.y + heightRef.current;
      const targetH = H_COLLAPSED + (panelRef.current?.offsetHeight ?? 0);
      if (Math.abs(targetH - heightRef.current) > 0.5) {
        await animateToHeight(targetH, bottom, pos.x);
        heightRef.current = targetH;
      }
    } finally {
      busyRef.current = false;
    }
  }

  async function saveKey() {
    const trimmed = key.trim();
    if (!trimmed) {
      setApiMsg({ ok: false, text: "请输入 API Key" });
      return;
    }
    setTesting(true);
    setApiMsg(null);
    try {
      await invoke("api_key_save", { key: trimmed });
      setConfigured(true);
      setKey("");
      setApiMsg({ ok: true, text: "已保存到 Windows 凭据管理器" });
    } catch (err) {
      setApiMsg({ ok: false, text: String(err) });
    } finally {
      setTesting(false);
    }
  }

  async function testConnection() {
    setTesting(true);
    setApiMsg(null);
    try {
      const info = await invoke<Balance>("api_test_connection");
      setBalance(info);
      setStatus("ok");
      setApiMsg({
        ok: true,
        text: info.is_available
          ? `连接成功，API Key 有效（${info.currency}）`
          : "连接成功，但账户当前不可用",
      });
    } catch (err) {
      setApiMsg({ ok: false, text: String(err) });
    } finally {
      setTesting(false);
    }
  }

  async function clearKey() {
    setTesting(true);
    setApiMsg(null);
    try {
      await invoke("api_key_clear");
      setConfigured(false);
      setBalance(null);
      setStatus("idle");
      setApiMsg({ ok: true, text: "已清除 API Key" });
    } catch (err) {
      setApiMsg({ ok: false, text: String(err) });
    } finally {
      setTesting(false);
    }
  }

  async function saveToken() {
    const trimmed = token.trim();
    if (!trimmed) {
      setOfficialMsg({ ok: false, text: "请输入 userToken" });
      return;
    }
    const quoted =
      (trimmed.startsWith('"') && trimmed.endsWith('"')) ||
      (trimmed.startsWith("'") && trimmed.endsWith("'"));
    const clean = quoted && trimmed.length > 1 ? trimmed.slice(1, -1) : trimmed;
    setTokenBusy(true);
    setOfficialMsg(null);
    try {
      // 先用输入框里的 token 校验一次，成功才落盘
      const data = await invoke<OfficialUsage>("official_usage_test", {
        token: clean,
      });
      await invoke("user_token_save", { token: clean });
      setTokenConfigured(true);
      setOfficial(data);
      setToken("");
      setOfficialMsg({ ok: true, text: "userToken 有效，已保存并同步" });
    } catch (err) {
      setOfficialMsg({ ok: false, text: String(err) });
    } finally {
      setTokenBusy(false);
    }
  }

  async function clearToken() {
    setTokenBusy(true);
    setOfficialMsg(null);
    try {
      await invoke("user_token_clear");
      setTokenConfigured(false);
      setOfficial(null);
      setOfficialMsg({ ok: true, text: "已清除 userToken" });
    } catch (err) {
      setOfficialMsg({ ok: false, text: String(err) });
    } finally {
      setTokenBusy(false);
    }
  }

  async function openLogin() {
    try {
      await invoke("open_deepseek_login");
      setOfficialMsg({
        ok: true,
        text: "已打开官网。登录后按 F12 → Console，粘贴执行 localStorage.getItem(\"userToken\")，把返回的长串填到下面输入框",
      });
    } catch (err) {
      setOfficialMsg({ ok: false, text: String(err) });
    }
  }

  async function copyTokenCmd() {
    const cmd = 'localStorage.getItem("userToken")';
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(cmd);
      } else {
        const ta = document.createElement("textarea");
        ta.value = cmd;
        document.body.appendChild(ta);
        ta.select();
        document.execCommand("copy");
        document.body.removeChild(ta);
      }
      setCopiedCmd(true);
      window.setTimeout(() => setCopiedCmd(false), 1500);
    } catch {
      setCopiedCmd(false);
    }
  }

  async function resetPosition() {
    setPosMsg(null);
    try {
      await invoke("reset_widget_position");
      setPosMsg("已恢复默认位置");
    } catch (err) {
      setPosMsg(String(err));
    }
  }

  function onBarPointerDown(e: React.PointerEvent) {
    if (e.button !== 0) return;
    dragRef.current = { sx: e.screenX, sy: e.screenY, started: false };
    e.currentTarget.setPointerCapture(e.pointerId);
  }

  async function onBarPointerMove(e: React.PointerEvent) {
    const d = dragRef.current;
    if (!d || d.started) return;
    if (Math.hypot(e.screenX - d.sx, e.screenY - d.sy) < DRAG_THRESHOLD_PX) return;
    d.started = true;
    try {
      await win.startDragging();
    } catch (err) {
      console.error("startDragging failed:", err);
      d.started = false;
    }
  }

  function onBarPointerUp() {
    const d = dragRef.current;
    dragRef.current = null;
    if (d && !d.started) void toggle();
  }

  return (
    <div className="card">
      <div
        className="bar"
        onPointerDown={onBarPointerDown}
        onPointerMove={onBarPointerMove}
        onPointerUp={onBarPointerUp}
        onPointerCancel={() => (dragRef.current = null)}
      >
        <span className="brand">DeepSeek</span>
        <span className="status">
          <i
            className={`dot${
              status === "ok" ? " ok" : status === "err" ? " err" : ""
            }`}
          />
          {!configured
            ? "未连接"
            : status === "updating"
              ? "更新中"
              : status === "ok"
                ? "正常"
                : status === "err"
                  ? "连接失败"
                  : "已配置"}
        </span>
        {ui.show_balance && (
          <span className="balance">{balance ? fmtMoney(balance) : "¥ —"}</span>
        )}
        <span className="chevron">›</span>
      </div>

      <div ref={panelRef} className={`panel${open ? " open" : ""}`}>
        <div className={`metrics${view === "settings" ? " hide" : ""}`}>
          {ui.show_balance && ui.show_today_cost && (
            <div className="pair">
              <Metric
                label="账户余额"
                value={balance ? fmtMoney(balance) : "¥ —"}
                sub={
                  balance && (balance.granted_balance || balance.topped_up_balance)
                    ? [
                        balance.granted_balance ? `赠 ¥${balance.granted_balance}` : "",
                        balance.topped_up_balance ? `充 ¥${balance.topped_up_balance}` : "",
                      ]
                        .filter(Boolean)
                        .join(" · ")
                    : undefined
                }
              />
              <Metric
                label="今日花费"
                value={
                  official
                    ? fmtCost(official.today_cost, official.currency)
                    : "未登录"
                }
              />
            </div>
          )}
          {ui.show_balance && !ui.show_today_cost && (
            <Metric
              label="账户余额"
              value={balance ? fmtMoney(balance) : "¥ —"}
            />
          )}
          {!ui.show_balance && ui.show_today_cost && (
            <Metric
              label="今日花费"
              value={
                official
                  ? fmtCost(official.today_cost, official.currency)
                  : "未登录"
              }
            />
          )}
          {ui.show_tokens && (
            <Metric
              label="今日 Tokens"
              value={
                official
                  ? fmtTokens0(official.today_tokens)
                  : usage.today_tokens > 0
                    ? fmtTokens(usage.today_tokens)
                    : "未登录"
              }
            />
          )}
          {ui.show_month_cost ? (
            <div className="pair">
              <Metric
                label="本月花费"
                value={
                  official
                    ? fmtCost(official.month_cost, official.currency)
                    : "未登录"
                }
              />
              <Metric label="当前模型" value={usage.current_model || "未登录"} />
            </div>
          ) : (
            <Metric label="当前模型" value={usage.current_model || "未登录"} />
          )}
          {ui.show_tokens && (
            <Metric
              label="本月 Tokens"
              value={
                official
                  ? fmtTokens0(official.month_tokens)
                  : usage.month_tokens > 0
                    ? fmtTokens(usage.month_tokens)
                    : "未登录"
              }
            />
          )}
        </div>
        <div className={`settings${view === "settings" ? " show" : ""}`}>
          <div className="settings-head">DeepSeek API Key</div>
          <p className="settings-hint">
            密钥只保存在 Windows 凭据管理器，不会写入项目文件。
          </p>
          <input
            className="key-input"
            type="password"
            autoComplete="off"
            spellCheck={false}
            placeholder="sk-…"
            value={key}
            onChange={(e) => setKey(e.currentTarget.value)}
          />
          <div className="settings-actions">
            <button
              className="set-btn primary"
              disabled={testing || !key.trim()}
              onClick={() => void saveKey()}
            >
              保存
            </button>
            <button
              className="set-btn"
              disabled={testing || !configured}
              onClick={() => void testConnection()}
            >
              测试连接
            </button>
            <button
              className="set-btn danger"
              disabled={testing || !configured}
              onClick={() => void clearKey()}
            >
              清除
            </button>
          </div>
          {apiMsg && (
            <div className={`api-msg ${apiMsg.ok ? "ok" : "err"}`}>{apiMsg.text}</div>
          )}
          <div className="set-divider" />
          <div className="settings-head">官方账单（可选）</div>
          <p className="settings-hint">
            未填 userToken 时，花费与 Token 显示“未登录”或本地统计；填好后会
            从官网账户直接读取今日/本月用量与花费，不再需要打开网页。
          </p>
          <div className="settings-actions">
            <button className="set-btn primary" onClick={() => void openLogin()}>
              打开 DeepSeek 官网
            </button>
          </div>
          <p className="settings-hint">
            获取 userToken：在官网登录后按 F12 → Console，复制下面命令回车，
            再把返回的引号内长串粘贴到这里（token 会过期，仅安全保存在
            Windows 凭据管理器）：
          </p>
          <div className="cmd-row">
            <code className="cmd-code">
              localStorage.getItem("userToken")
            </code>
            <button className="cmd-copy" onClick={() => void copyTokenCmd()}>
              {copiedCmd ? "已复制" : "复制"}
            </button>
          </div>
          <input
            className="key-input"
            type="password"
            autoComplete="off"
            spellCheck={false}
            placeholder="粘贴 userToken"
            value={token}
            onChange={(e) => setToken(e.currentTarget.value)}
          />
          <div className="settings-actions">
            <button
              className="set-btn primary"
              disabled={tokenBusy || !token.trim()}
              onClick={() => void saveToken()}
            >
              {tokenConfigured ? "更换并同步" : "保存并同步"}
            </button>
            <button
              className="set-btn danger"
              disabled={tokenBusy || !tokenConfigured}
              onClick={() => void clearToken()}
            >
              清除
            </button>
          </div>
          {officialMsg && (
            <div className={`api-msg ${officialMsg.ok ? "ok" : "err"}`}>
              {officialMsg.text}
            </div>
          )}

          <div className="set-divider" />
          <div className="settings-head">Widget 显示</div>
          <ToggleRow
            label="启动时显示"
            checked={ui.start_visible}
            onChange={() => saveUi({ start_visible: !ui.start_visible })}
          />
          <ToggleRow
            label="显示余额"
            checked={ui.show_balance}
            onChange={() => saveUi({ show_balance: !ui.show_balance })}
          />
          <ToggleRow
            label="显示 Token（本地统计）"
            checked={ui.show_tokens}
            onChange={() => saveUi({ show_tokens: !ui.show_tokens })}
          />
          <ToggleRow
            label="显示今日花费"
            checked={ui.show_today_cost}
            onChange={() => saveUi({ show_today_cost: !ui.show_today_cost })}
          />
          <ToggleRow
            label="显示本月花费"
            checked={ui.show_month_cost}
            onChange={() => saveUi({ show_month_cost: !ui.show_month_cost })}
          />

          <div className="set-divider" />
          <div className="settings-head">刷新</div>
          <div className="interval-row">
            <span>余额（秒）</span>
            <input
              className="interval-input"
              type="number"
              min={5}
              value={ui.balance_interval_s}
              onChange={(e) =>
                saveUi({
                  balance_interval_s: Math.max(5, Number(e.currentTarget.value) || 60),
                })
              }
            />
          </div>
          <div className="interval-row">
            <span>本地用量（分钟）</span>
            <input
              className="interval-input"
              type="number"
              min={1}
              value={ui.usage_interval_min}
              onChange={(e) =>
                saveUi({
                  usage_interval_min: Math.max(1, Number(e.currentTarget.value) || 5),
                })
              }
            />
          </div>

          <div className="set-divider" />
          <div className="settings-head">位置</div>
          <div className="settings-actions">
            <button className="set-btn" onClick={() => void resetPosition()}>
              恢复默认位置
            </button>
          </div>
          {posMsg && <div className="api-msg ok">{posMsg}</div>}

          <div className="set-divider" />
          <div className="settings-head">本地统计代理</div>
          <p className="settings-hint">
            把 DeepSeek 客户端的 base_url 改成下面地址，经过它的调用会自动记账：
          </p>
          <input
            className="key-input url-box"
            readOnly
            value={proxyUrl}
            onFocus={(e) => e.currentTarget.select()}
          />

          <div className="set-divider" />
          <div className="about">DeepSeek Status Widget · Version {APP_VERSION}</div>
          <div className="back-link" onClick={() => void switchView("stats")}>
            ‹ 返回数据
          </div>
        </div>
        <div className="footer">
          <span
            className="btn"
            data-no-drag
            onClick={() => void refreshAll()}
          >
            {status === "updating" ? "⋯ 更新中" : "↻ 刷新"}
          </span>
          {!official &&
            (usage.today_tokens > 0 || usage.month_tokens > 0) && (
            <span className="local-badge">本地统计</span>
          )}
          <span
            className="btn"
            data-no-drag
            onClick={() => void switchView("settings")}
          >
            ⚙ 设置
          </span>
        </div>
      </div>
    </div>
  );
}

export default App;
