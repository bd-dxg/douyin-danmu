//! 抖音扫码登录：加载直播首页，扫码后把 Cookie 收下来
//!
//! 为什么必须登录：抖音只在**登录态**下推礼物消息（实测同一房间：匿名 180 秒 0 条礼物、
//! 带登录 120 秒 8 条）；弹幕 / 进场 / 关注 / 点赞匿名就能收。
//!
//! **登录跑在独立的 helper 子进程里**（入口 `login_helper.rs`）：抖音首页是重度 SPA，
//! 会让 WebView2 自动播放视频流，GPU 进程峰值 1 GB 上下；登录窗口销毁后还有约 84 MB
//! 的 GPU 缓存残留（进程级缓存，要等系统内存压力才回收）。子进程退出是唯一能确定
//! 全额归还内存的方式，主程序全程不加载抖音首页，连那点残留都不会沾上。
//!
//! 本模块只留「登录页本身」：建窗 / 读 Cookie / 等扫码；
//! helper 进程入口在 `login_helper.rs`，主程序侧的编排在 `commands::douyin_login_open`。
//!
//! 登录成功 = cookie 里出现 `sessionid`；轮询 5 分钟没等到就放弃，让用户重试。

use std::path::PathBuf;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// 登录窗口的 label（helper 进程里现建、登录完随进程一起消失）
pub const LOGIN_WINDOW_LABEL: &str = "login";
/// 登录页：直播间首页（顶部栏右侧有「登录」入口，扫码后自动跳回）
///
/// 注意别换成窄窗口或 mobile UA：抖音的顶部栏在窄视口下会把登录入口整个藏起来。
const LOGIN_URL: &str = "https://live.douyin.com/";
/// 判定「已登录」的 cookie 名（抖音 passport 登录后下发）
const SESSION_COOKIE: &str = "sessionid";
/// 轮询间隔与总超时
const POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// 登录页专用的 WebView2 数据目录
///
/// 不能和主程序共用 `EBWebView`：WebView2 不允许两个环境（两个进程）指向同一个
/// user data folder。独立目录还给「退出登录」留了一个明确的清理目标（见 `commands::logout`）。
pub fn login_webview_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_local_data_dir()
        .map(|d| d.join("login-webview"))
        .map_err(|e| format!("取本地数据目录失败：{e}"))
}

/// 建登录窗口（宽度不能小：抖音页面是响应式的，窄到 ~600px 以下顶部栏会折叠、
/// 「登录」入口直接不渲染 —— 实测 520 宽的窗口里根本找不到登录按钮）
pub fn create_login_window(app: &AppHandle) -> Result<WebviewWindow, String> {
    let url: Url = LOGIN_URL
        .parse()
        .map_err(|e| format!("登录页地址无效：{e}"))?;
    let dir = login_webview_dir(app)?;
    WebviewWindowBuilder::new(app, LOGIN_WINDOW_LABEL, WebviewUrl::External(url))
        .title("抖音扫码登录 · 点右上角「登录」就能扫码（成功后本窗口自动关）")
        .inner_size(1180.0, 820.0)
        .min_inner_size(1000.0, 680.0)
        .data_directory(dir)
        // 不让抖音首页自动播放视频流：WebView2 默认是允许自动播放的，首页一开就出声，
        // 同时把 GPU 显存顶到 1 GB 上下。改成「需要用户手势」后，用户自己点一下页面
        // 照样能播，扫码登录完全不受影响。
        //
        // 注意：additional_browser_args 会**整串替换** wry 的默认参数，所以默认那三个
        // 必须自己带上（见 tauri 源码 `WebviewWindowBuilder::additional_browser_args` 的 Warning）。
        .additional_browser_args(
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
             --autoplay-policy=document-user-activation-required",
        )
        .center()
        .build()
        .map_err(|e| format!("打开登录窗口失败：{e}"))
}

/// 等扫码完成，返回 Cookie 串
///
/// 三种结束方式：扫码成功 → `Ok(cookie)`；窗口被用户关掉 → `Err`；超时 → `Err`。
pub async fn wait_for_login(app: &AppHandle, win: &WebviewWindow) -> Result<String, String> {
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        // 用户自己把窗口关了 = 放弃登录
        if app.get_webview_window(LOGIN_WINDOW_LABEL).is_none() {
            return Err("登录窗口已关闭，未完成登录".to_owned());
        }
        match read_douyin_cookie(win) {
            Ok(Some(cookie)) => return Ok(cookie),
            Ok(None) => {}
            Err(e) => return Err(e),
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "登录超时（{}s 内没检测到登录），请重试",
                LOGIN_TIMEOUT.as_secs()
            ));
        }
    }
}

/// 读抖音域下的 cookie 串；还没登录（没有 `sessionid`）时返回 None
fn read_douyin_cookie(win: &WebviewWindow) -> Result<Option<String>, String> {
    let cookies = win
        .cookies()
        .map_err(|e| format!("读取登录 Cookie 失败：{e}"))?;
    if !cookies
        .iter()
        .any(|c| c.name() == SESSION_COOKIE && !c.value().is_empty())
    {
        return Ok(None);
    }
    // 带域名的只留抖音域，**没有 Domain 属性的 host-only cookie 必须保留**：
    // 抖音的会话 cookie 大多是 host-only（`sessionid` / `ttwid` 都可能是），
    // 它们没有 domain 字段，一并滤掉就会回传空串（release 实测踩过）。
    let pairs: Vec<String> = cookies
        .iter()
        .filter(|c| c.domain().is_none_or(|d| d.ends_with("douyin.com")))
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect();
    // 兜底：判出登录态却收不出 Cookie（域过滤异常）时宁可报错，也不回一个空串
    // 让主程序把它当成"登录成功"
    if pairs.is_empty() {
        return Err("读到登录态但 Cookie 串为空，请重试".to_owned());
    }
    Ok(Some(pairs.join("; ")))
}
