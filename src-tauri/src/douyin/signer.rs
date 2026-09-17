//! 隐藏 WebView2 签名器：把 webmssdk 的 `getSign` 包成 [`Signer`]
//!
//! 为什么走 WebView2 而不是 node 子进程：用户零依赖，Tauri 本来就带 WebView2
//! （DanmuFree 用 node 是因为它没有内置浏览器）。
//!
//! 请求/回包为什么绕一圈：`eval` 只负责触发，**拿不到 JS 的返回值**，所以页面把结果
//! 经命令 `sign_reply` 送回来，这里按 id 用 oneshot 对上一次请求。
//!
//! 页面是构建产物里的 `sign.html`（加载 `douyin-sign.js` = 探针验证过的 `sign_browser.js`）。
//! 同一个窗口后面还要兼做扫码登录（阶段 5b），登录时它被导航到抖音登录页，
//! 那期间签名会回 `NOT_READY`，所以「未就绪」是**重试**而不是报错。

use super::sign::{SignError, SignFuture, Signer};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;

/// 签名窗口的 label（登录复用同一个窗口）
pub const SIGN_WINDOW_LABEL: &str = "sign";

/// 单次签名等待上限（实测 webmssdk 一次约 10–30 ms）
const SIGN_TIMEOUT: Duration = Duration::from_secs(3);
/// 「签名页还没就绪」的重试次数与间隔：启动后立刻点「连接」会撞上页面还在加载
const NOT_READY_RETRY: usize = 10;
const NOT_READY_INTERVAL: Duration = Duration::from_millis(200);
/// 页面回包里表示「还没就绪」的哨兵值
const NOT_READY: &str = "NOT_READY";

/// 签名页的回包
enum Reply {
    Ok(String),
    NotReady,
    Failed(String),
}

#[derive(Clone)]
pub struct WebviewSigner {
    inner: Arc<Inner>,
}

struct Inner {
    app: AppHandle,
    next_id: AtomicU64,
    /// 在途请求：id → 回包发送端（超时会清掉，迟到回包直接丢弃）
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
}

/// 创建隐藏的签名窗口。**启动时就建**：第一次点「连接」时不必再等页面加载。
pub fn create_window(app: &AppHandle) -> tauri::Result<()> {
    WebviewWindowBuilder::new(app, SIGN_WINDOW_LABEL, WebviewUrl::App("sign.html".into()))
        .title("douyin-danmu sign")
        .inner_size(480.0, 320.0)
        .visible(false)
        .skip_taskbar(true)
        .build()?;
    Ok(())
}

impl WebviewSigner {
    pub fn new(app: AppHandle) -> Self {
        Self {
            inner: Arc::new(Inner {
                app,
                next_id: AtomicU64::new(1),
                pending: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// 签名页回包入口（命令 [`douyin_sign_reply`] 调它）
    pub fn resolve(&self, id: u64, bogus: Option<String>, error: Option<String>) {
        // 超时后到达的迟到回包：表里已经没有它了，直接丢
        let Some(tx) = self.inner.pending.lock().unwrap().remove(&id) else {
            return;
        };
        let reply = match (bogus, error) {
            (Some(b), _) if !b.is_empty() => Reply::Ok(b),
            (_, Some(e)) if e == NOT_READY => Reply::NotReady,
            (_, Some(e)) => Reply::Failed(e),
            // 漏下的只可能是 (Some(""), None)：回了空 X-Bogus 又没说原因
            _ => Reply::Failed("签名页既没回 X-Bogus 也没回错误".into()),
        };
        let _ = tx.send(reply);
    }

    /// 自检：跑一次固定 stub 的签名，把结果交出去（供启动时打日志）
    pub async fn self_check(&self) -> Result<String, SignError> {
        self.sign("c1180e560e13ce8c5567b4002dbac516".to_owned())
            .await
    }
}

impl Signer for WebviewSigner {
    fn sign(&self, md5: String) -> SignFuture {
        let inner = self.inner.clone();
        Box::pin(async move {
            let mut not_ready = 0;
            loop {
                match sign_once(&inner, &md5).await {
                    Reply::Ok(bogus) => return Ok(bogus),
                    Reply::Failed(e) => return Err(SignError(e)),
                    Reply::NotReady if not_ready < NOT_READY_RETRY => {
                        not_ready += 1;
                        tokio::time::sleep(NOT_READY_INTERVAL).await;
                    }
                    Reply::NotReady => {
                        return Err(SignError(
                            "签名页一直没就绪（douyin-sign.js 缺失？或窗口被导航走了）".into(),
                        ))
                    }
                }
            }
        })
    }
}

/// 一次签名请求：eval 触发 → 等回包
async fn sign_once(inner: &Arc<Inner>, md5: &str) -> Reply {
    let Some(win) = inner.app.get_webview_window(SIGN_WINDOW_LABEL) else {
        return Reply::Failed("签名窗口不存在（创建失败？）".into());
    };
    let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    inner.pending.lock().unwrap().insert(id, tx);

    // md5 是我们自己算的 16 进制，仍走 JSON 转义塞进字面量，免得将来换了入参出岔子
    let stub = serde_json::to_string(md5).unwrap_or_else(|_| "\"\"".into());
    let js = format!(
        "(function(){{\
           var done=function(o){{window.__TAURI_INTERNALS__.invoke('douyin_sign_reply',Object.assign({{id:{id}}},o));}};\
           if(typeof window.__douyinSign==='function'){{window.__douyinSign({id},{stub});}}\
           else{{done({{error:'{NOT_READY}'}});}}\
         }})()"
    );
    if let Err(e) = win.eval(&js) {
        inner.pending.lock().unwrap().remove(&id);
        return Reply::Failed(format!("eval 到签名页失败：{e}"));
    }

    match tokio::time::timeout(SIGN_TIMEOUT, rx).await {
        Ok(Ok(reply)) => reply,
        Ok(Err(_)) => Reply::Failed("签名页回包通道断了".into()),
        Err(_) => {
            inner.pending.lock().unwrap().remove(&id);
            Reply::Failed(format!(
                "签名页 {}s 没回结果（douyin_sign_reply 未注册 / 页面没加载）",
                SIGN_TIMEOUT.as_secs()
            ))
        }
    }
}

/// 签名页回包（页面里 `src/sign.ts` 调，不是给用户用的）
///
/// 注意命令名 = 函数名（`src/sign.ts` 里写的就是 `douyin_sign_reply`）；
/// 改这里必须同步改那边，否则签名只能等超时。
#[tauri::command]
pub fn douyin_sign_reply(app: AppHandle, id: u64, bogus: Option<String>, error: Option<String>) {
    app.state::<WebviewSigner>().resolve(id, bogus, error);
}
