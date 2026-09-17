//! 抖音扫码登录：开一个可见窗口加载直播首页，扫码后把 Cookie 收下来
//!
//! 为什么必须登录：抖音只在**登录态**下推礼物消息（实测同一房间：匿名 180 秒 0 条礼物、
//! 带登录 120 秒 8 条）；弹幕 / 进场 / 关注 / 点赞匿名就能收。
//!
//! 为什么单独开一个窗口、而不是复用签名窗口：签名窗口的页面一旦被导航走就签不了名
//! （`signer.rs` 会把 NOT_READY 一路重试到失败）。两个窗口共用同一个 WebView2 cookie 库，
//! 所以登录完在哪个窗口读 `cookies()` 都一样。
//!
//! 登录成功 = cookie 里出现 `sessionid`；轮询 5 分钟没等到就放弃，让用户重试。

use crate::config;
use crate::state::AppState;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// 登录窗口的 label（每次登录现建、收尾即销毁）
const LOGIN_WINDOW_LABEL: &str = "douyin-login";
/// 登录页：直播间首页（顶部栏右侧有「登录」入口，扫码后自动跳回）
///
/// 注意别换成窄窗口或 mobile UA：抖音的顶部栏在窄视口下会把登录入口整个藏起来。
const LOGIN_URL: &str = "https://live.douyin.com/";
/// 判定「已登录」的 cookie 名（抖音 passport 登录后下发）
const SESSION_COOKIE: &str = "sessionid";
/// 轮询间隔与总超时
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// 打开登录窗口并等扫码完成（命令一直悬着，直到成功 / 超时 / 窗口被关）
///
/// 成功时 cookie 经 DPAPI 加密落盘（`config::save_auth`）并同步进内存登录态，
/// 之后连接流程每次会话前都会实时读它（重连期间登录/退出都生效）。
#[tauri::command]
pub async fn douyin_login_open(app: AppHandle) -> Result<(), String> {
    let url = LOGIN_URL
        .parse()
        .map_err(|e| format!("登录页地址无效：{e}"))?;
    let win = WebviewWindowBuilder::new(&app, LOGIN_WINDOW_LABEL, WebviewUrl::External(url))
        .title("抖音扫码登录 · 点右上角「登录」就能扫码（成功后本窗口自动关）")
        // 宽度不能小：抖音页面是响应式的，窄到 ~600px 以下顶部栏会折叠、
        // 「登录」入口直接不渲染（实测：520 宽的窗口里根本找不到登录按钮）
        .inner_size(1180.0, 820.0)
        .min_inner_size(1000.0, 680.0)
        .center()
        .build()
        .map_err(|e| format!("打开登录窗口失败：{e}"))?;
    log::info!(
        "[login] 已打开抖音登录页，等待扫码（最多 {}s）",
        LOGIN_TIMEOUT.as_secs()
    );

    let deadline = Instant::now() + LOGIN_TIMEOUT;
    let result = loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        // 用户自己把窗口关了 = 放弃登录
        if app.get_webview_window(LOGIN_WINDOW_LABEL).is_none() {
            break Err("登录窗口已关闭，未完成登录".to_owned());
        }
        match douyin_cookie(&win) {
            Ok(Some(cookie)) => break Ok(cookie),
            Ok(None) => {}
            Err(e) => break Err(e),
        }
        if Instant::now() >= deadline {
            break Err(format!(
                "登录超时（{}s 内没检测到登录），请重试",
                LOGIN_TIMEOUT.as_secs()
            ));
        }
    };

    // 收尾：不管成败都销毁窗口，避免留下一个还开着登录页的窗口
    let _ = win.destroy();

    let cookie = result?;
    let auth = config::AuthInfo { cookies: cookie };
    config::save_auth(&app, &auth)?;
    *app.state::<AppState>().auth.lock().unwrap() = Some(auth);
    log::info!("[login] 抖音登录成功，Cookie 已加密落盘（礼物消息从下次连接开始可收）");
    Ok(())
}

/// 读抖音域下的 cookie 串；还没登录（没有 `sessionid`）时返回 None
fn douyin_cookie(win: &tauri::WebviewWindow) -> Result<Option<String>, String> {
    let cookies = win
        .cookies()
        .map_err(|e| format!("读取登录 Cookie 失败：{e}"))?;
    if !cookies
        .iter()
        .any(|c| c.name() == SESSION_COOKIE && !c.value().is_empty())
    {
        return Ok(None);
    }
    // 只带抖音域下的：WebView2 的 cookie 库是全应用共用的
    let pairs: Vec<String> = cookies
        .iter()
        .filter(|c| c.domain().is_some_and(|d| d.ends_with("douyin.com")))
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect();
    Ok(Some(pairs.join("; ")))
}
