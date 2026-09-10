# DeepSeek Status Widget

一个跟随 **Codex 桌面窗口** 的 Windows 透明悬浮小组件，用来快速查看 DeepSeek 账户余额、Token 用量与花费。它本身是一个独立的无边框透明窗口，默认贴在 Codex 左下角、用户账户栏的正上方，视觉上看起来像是 Codex 原生界面的一部分。

> 它不是 Codex 插件、不注入 Codex、不读取 Codex 内部 DOM 或对话内容，也绝不读取你的 GitHub / Codex 登录数据。

## 功能

- **透明悬浮 Widget**：无边框、深色、可收起/展开、可拖拽
- **跟随 Codex**：自动寻找 Codex 主窗口并贴靠；移动、缩放、最大化、最小化、恢复都会跟随；最小化/关闭时隐藏
- **位置记忆**：拖动后记住相对 Codex 的偏移，重启恢复
- **账户余额**：通过官方 `GET /user/balance` 展示总余额（赠送 / 充值细分）
- **官方账单（可选）**：填入官网 `userToken` 后，读取今日 / 本月 Token 与花费（详细见下）
- **设置页**：API Key、官方账单 token、显示项开关、刷新间隔、恢复默认位置
- **随 Windows 启动**：登录后它一直在后台待命，Codex 一打开就自动弹出并贴靠
- **安全保存**：密钥与 token 都存进 Windows 凭据管理器，不落盘明文

## 数据与安全（重要）

- **API Key 与 userToken 只保存在 Windows 凭据管理器（Credential Manager）里**，不写入任何项目文件，也不会发送到本工具之外的服务器。
- 官方“今日 / 本月”用量与花费来自 `platform.deepseek.com` 的几个**私有接口**，需要你在网页控制台取一次 `userToken` 手动粘贴；这些接口**可能随时变更**，失效时设置页会提示你重新粘贴。
- `userToken` 是网页登录态（session），**可能过期**；过期时需要重新复制。为安全起见，本工具**不会**保存你的登录密码、不会保存 Cookie、也不会自动替你登录。
- **不放心？** 本项目全部源码都是开源的，你可以直接查看，验证数据确实只保存在本地。
- 官方数据的“今日”按平台接口的 UTC 天数切分，因此在**北京时间凌晨 0:00–8:00**，国内看到的“今日”可能与官网页面略有偏差。

## 如何获取 userToken（用于官方账单）

1. 在小工具设置里点击“打开 DeepSeek 官网”，登录。
2. 在网页按 `F12`，切到 **Console（控制台）**，先输入 `allow pasting` 回车，然后粘贴下面这行回车：

   ```js
   localStorage.getItem("userToken")
   ```

3. 把返回的长串（如果是最新格式，复制 `{"value":"..."}` 整段即可）粘回小工具的输入框，点“保存并同步”。

小工具会先拿它去官网验证一次，有效才保存。之后会按设置的刷新间隔自动更新“今日 / 本月”的 Token 与花费。

## 技术栈

- 前端：Tauri 2 + React 19 + TypeScript + Vite
- 后端：Rust（Win32 FFI 找窗口 / 移动窗口，无额外插件）
- 平台：Windows

## 开发 & 构建

需要 [Node.js](https://nodejs.org/) 和 [Rust](https://www.rust-lang.org/tools/install)：

```bash
npm install
npm run tauri dev    # 开发
npm run tauri build  # 打包安装程序
```

## License

MIT © DeepSeek Status Widget contributors
