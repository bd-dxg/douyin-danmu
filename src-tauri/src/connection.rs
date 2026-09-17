//! 直播间连接：命令入口 + 断线自动重连
//!
//! 连接流程：解析短号（web_rid → 真实 room_id + ttwid）→ 弹幕会话（签名 + WS，
//! 断线自动退避重连）→ 收尾清理状态并上报。
//!
//! 直播间的「号」有两个：用户输入的是**短号**（浏览器地址 `live.douyin.com/` 后面那串，
//! 如 765790908625），WS 只认 19 位**真实 room_id**，由 `resolver` 换一次。
//! 对外一律用短号（前端展示与最近房间都用它），真实 room_id 只在会话内部用。

use crate::config;
use crate::douyin::event::DouyinEvent;
use crate::douyin::resolver;
use crate::douyin::signer::WebviewSigner;
use crate::douyin::ws::{self, SessionConfig, SessionHandler};
use crate::gift;
use crate::state::{clear_danmaku_ticks, AppState, RoomStatus};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio_util::sync::CancellationToken;

/// 连接直播间（参数是直播间**短号**，不是 19 位 room_id）
#[tauri::command]
pub(crate) async fn connect_room(
    app: AppHandle,
    state: State<'_, AppState>,
    web_rid: String,
) -> Result<(), String> {
    let web_rid = web_rid.trim().to_owned();
    if web_rid.is_empty() || !web_rid.chars().all(|c| c.is_ascii_digit()) {
        return Err("直播间号应为纯数字（浏览器地址 live.douyin.com/ 后面那串）".into());
    }
    {
        let mut st = state.status.lock().unwrap();
        if !matches!(*st, RoomStatus::Disconnected) {
            return Err("已有连接，请先断开".into());
        }
        *st = RoomStatus::Connecting {
            web_rid: web_rid.clone(),
        };
    }
    let _ = app.emit(
        "room-status",
        json!({ "state": "connecting", "webRid": web_rid }),
    );

    let cancel = CancellationToken::new();
    *state.cancel.lock().unwrap() = Some(cancel.clone());

    // 登录态由 spawn_connection 每次会话前实时读取（重连期间用户可登录/退出）
    spawn_connection(app.clone(), web_rid, cancel);
    Ok(())
}

/// 断开连接
#[tauri::command]
pub(crate) fn disconnect_room(app: AppHandle, state: State<'_, AppState>) {
    *state.status.lock().unwrap() = RoomStatus::Disconnected;
    clear_danmaku_ticks(&app);
    gift::clear(&app);
    if let Some(token) = state.cancel.lock().unwrap().take() {
        token.cancel();
    }
    let _ = app.emit("room-status", json!({ "state": "disconnected" }));
}

/// 查询当前连接状态（Overlay 轮询兜底，防事件丢失）
#[tauri::command]
pub(crate) fn get_connection_status(app: AppHandle) -> serde_json::Value {
    let st = app.state::<AppState>();
    let st = st.status.lock().unwrap();
    match &*st {
        RoomStatus::Disconnected => json!({ "state": "disconnected" }),
        RoomStatus::Connecting { web_rid } => {
            json!({ "state": "connecting", "webRid": web_rid })
        }
        RoomStatus::Connected { web_rid } => {
            json!({ "state": "connected", "webRid": web_rid })
        }
    }
}

/// 弹幕会话失败时最多重连几次（退避序列见 `ws::reconnect_delay`，累计约 4 分钟）
const MAX_CONNECT_ATTEMPTS: u32 = 10;

/// 会话事件出口：把解出的抖音事件分发给各个消费者
///
/// 实现在这里而不是 `ws.rs`：`ws` 只负责一帧一帧地收发，不认识 tts / 礼物 / 欢迎这几块。
struct RoomHandler {
    app: AppHandle,
    web_rid: String,
}

impl SessionHandler for RoomHandler {
    fn on_connected(&self) {
        crate::state::on_room_connected(&self.app, self.web_rid.clone());
        log::info!("[douyin] 房间 {} 弹幕会话已建立", self.web_rid);
        let _ = self.app.emit(
            "room-status",
            json!({ "state": "connected", "webRid": self.web_rid }),
        );
    }

    fn on_event(&self, event: DouyinEvent) {
        match event {
            DouyinEvent::Danmaku(d) => {
                // 朗读是与显示并列的独立消费者（关闭朗读不影响弹幕窗）
                crate::state::record_danmaku(&self.app);
                crate::tts::on_danmaku(&self.app, &d);
                let _ = self.app.emit("danmaku", d);
            }
            // 打赏：礼物列表与礼物朗读两个独立消费者（门槛与开关各自配置）
            DouyinEvent::Backing(b) => {
                crate::tts::on_backing(&self.app, &b);
                gift::on_backing(&self.app, b);
            }
            // 欢迎类：量级比弹幕大，先过去重与限速再广播
            DouyinEvent::Welcome(w) => crate::welcome::on_welcome(&self.app, w),
        }
    }
}

/// 完整连接流程：解析短号 → 弹幕会话（断线自动退避重连）→ 收尾清理状态并上报
///
/// 解析失败不重试（房间不存在/未开播时重试无意义）；会话断开后按
/// `ws::reconnect_delay` 退避静默自动重连（重连期间前端保持已连接语义，断流抖动自动恢复），
/// 超过 `MAX_CONNECT_ATTEMPTS` 次仍失败才报 error；签名失败（`retryable() == false`）
/// 立即报错 —— 那是环境问题，重试只会把同一次失败再跑一遍。
/// 用户可随时断开（取消令牌中止重试）。
fn spawn_connection(app: AppHandle, web_rid: String, cancel: CancellationToken) {
    tauri::async_runtime::spawn(async move {
        // 解析阶段全是网络请求，超时给短一点：卡住时让用户早看到错误而不是干等
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        // 每次会话前实时读取登录态（礼物必须登录态才推；游客能收弹幕/进场/关注/点赞）
        let login_cookie = {
            let st = app.state::<AppState>();
            let auth = st.auth.lock().unwrap();
            auth.as_ref().map(|a| a.cookies.clone())
        };

        // 1. 短号 → 真实 room_id + ttwid（ttwid 是 WS 握手的必要 cookie）
        let info = match resolver::resolve(&client, &web_rid, login_cookie.as_deref()).await {
            Ok(info) => info,
            Err(e) => {
                finish_connection(&app, &cancel, Err(e.to_string()));
                return;
            }
        };
        log::info!(
            "[douyin] 房间 {} 解析出 room_id={}（主播「{}」，{}）",
            web_rid,
            info.room_id,
            info.uname.as_deref().unwrap_or("?"),
            if info.live { "直播中" } else { "当前未在直播" }
        );
        if !info.live {
            log::warn!(
                "[douyin] 房间 {} 当前不是直播中，可能收不到弹幕/礼物（已结束或未开播）",
                web_rid
            );
        }

        // 2. 记入最近房间（面包屑显示主播名，拿不到时回退房间号）
        let _ = config::save_recent_room(
            &app,
            &config::RecentRoom {
                room_id: web_rid.clone(),
                uname: info.uname.clone(),
            },
        );
        let _ = app.emit("recent-rooms-changed", ());

        // audience 标识：整条连接复用（重连不必换，也没意义）
        let user_unique_id = ws::generate_user_unique_id();
        let signer = app.state::<WebviewSigner>().inner().clone();
        let handler = RoomHandler {
            app: app.clone(),
            web_rid: web_rid.clone(),
        };

        // 3. 会话循环：断线退避重连，直到成功 / 用户断开 / 重试耗尽 / 撞上不可重试错误
        let mut attempt = 0u32;
        let result: Result<(), String> = loop {
            if cancel.is_cancelled() {
                break Ok(());
            }
            let cfg = SessionConfig {
                room_id: &info.room_id,
                cookie: &info.cookie,
                user_unique_id: &user_unique_id,
            };
            match ws::run_ws_session(&cfg, &signer, &handler, cancel.clone()).await {
                Ok(()) => break Ok(()), // 被取消，正常结束
                Err(e) => {
                    if !e.retryable() {
                        break Err(e.to_string());
                    }
                    attempt += 1;
                    if attempt >= MAX_CONNECT_ATTEMPTS {
                        break Err(format!("{e}（已重连 {attempt} 次仍失败）"));
                    }
                    let backoff = ws::reconnect_delay(attempt);
                    log::warn!(
                        "[douyin] 会话断开（{e}），{}s 后重连（第 {attempt} 次）",
                        backoff.as_secs()
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = cancel.cancelled() => break Ok(()),
                    }
                }
            }
        };

        finish_connection(&app, &cancel, result);
    });
}

/// 连接任务收尾：仅当自己仍是当前连接任务（取消令牌未被换走）时重置状态并上报
///
/// disconnect_room 会 take 令牌、新连接会替换令牌——若收尾时状态里存的令牌
/// 已不是自己，说明已被断开或已有新连接接管，绝不能重置状态，否则会把新连接的
/// Connected 误清为 Disconnected（「断开 → 立即重连」时序下的旧任务竞态）。
fn finish_connection(app: &AppHandle, cancel: &CancellationToken, result: Result<(), String>) {
    let st = app.state::<AppState>();
    let is_current = st.cancel.lock().unwrap().as_ref() == Some(cancel);
    if !is_current {
        return;
    }
    *st.status.lock().unwrap() = RoomStatus::Disconnected;
    clear_danmaku_ticks(app);
    gift::clear(app);
    match result {
        Ok(()) => {
            let _ = app.emit("room-status", json!({ "state": "disconnected" }));
        }
        Err(msg) => {
            log::error!("[douyin] 连接结束: {msg}");
            let _ = app.emit("room-status", json!({ "state": "error", "message": msg }));
        }
    }
}
