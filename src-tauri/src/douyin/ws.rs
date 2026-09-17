//! 抖音弹幕 WebSocket 会话：签名 → 升级 WS → 收帧 → 回 ack → 心跳
//!
//! 与 B 站（`bilibili/ws.rs`）的关键差异：
//! - 签名不是请求头而是 URL 上的 `signature` 参数，且签名要跑 webmssdk 的 JS（见 `sign.rs`）
//! - 握手必须带 `ttwid`（`resolver` 已把它并进 cookie），缺了会 `HTTP 200 + named cookie not present`
//! - **收到帧必须回 ack**：不回服务端每 1–2 秒重推同一帧、cursor 不推进
//! - 没有「认证回包」：HTTP 101 就是成功，收到 101 即视为已连接
//!
//! 断线重连不在这里：`connection.rs`（阶段 6）按 [`WsError::retryable`] 与
//! [`reconnect_delay`] 驱动 [`run_ws_session`] 的循环，本模块只负责「一次会话」。

use crate::douyin::event::DouyinEvent;
use crate::douyin::parser;
use crate::douyin::proto;
use crate::douyin::sign::{self, Signer};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

/// 弹幕 WS 域名。**必须带 `-ws-web-`**（`webcast3-ws-web-lf`），换别的子域握不上手
const WS_HOST: &str = "wss://webcast3-ws-web-lf.douyin.com/webcast/im/push/v2/";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const ORIGIN: &str = "https://live.douyin.com";

/// 心跳间隔。实测 40 秒不心跳也不被踢，取探针与 DanmuFree 的 10 秒留足余量
const HEARTBEAT_SECS: u64 = 10;
/// 建连超时：域名被墙/DNS 卡住时快速失败，交给重连退避，而不是挂在 connect 里
const CONNECT_TIMEOUT_SECS: u64 = 10;
/// 重连退避上限（秒）
const RECONNECT_MAX_BACKOFF_SECS: u64 = 64;

/// 一次会话需要的三个外部输入
pub struct SessionConfig<'a> {
    /// 真实 room_id（19 位长号，短号要先用 `resolver` 换）
    pub room_id: &'a str,
    /// 含 `ttwid` 的完整 cookie（`resolver::resolve` 的产物，登录态可选）
    pub cookie: &'a str,
    /// audience 标识（19 位数字，见 [`generate_user_unique_id`]）
    pub user_unique_id: &'a str,
}

/// 会话回调：由 `connection.rs`（阶段 6）实现，把事件分发给 tts / gift / welcome
pub trait SessionHandler: Send + Sync {
    /// 握手成功（HTTP 101）时调用一次，用于把状态机切到「已连接」
    fn on_connected(&self);
    /// 每解出一条消息调用一次
    fn on_event(&self, event: DouyinEvent);
}

/// 会话失败的原因。分类不是为了好看：它决定 `connection.rs` 要不要重连。
#[derive(Debug)]
pub enum WsError {
    /// 签名失败（WebView2 未就绪 / webmssdk 没吐 X-Bogus）。
    ///
    /// **不重试**：签名失败是「环境问题」，退避重试只会把同一次失败再跑一遍，
    /// 而每次重试都要拉起一轮 IPC + JS 执行，等于把错误放大成风暴。
    Sign(String),
    /// 握手被拒：签名错、cookie 缺 ttwid、域名/参数改了
    Handshake(String),
    /// 建连 / 读写 / 心跳失败：网络抖动，值得退避重连
    Transport(String),
}

impl WsError {
    /// 是否值得退避重连
    pub fn retryable(&self) -> bool {
        !matches!(self, WsError::Sign(_))
    }
}

impl std::fmt::Display for WsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WsError::Sign(e) => write!(f, "签名失败：{e}"),
            WsError::Handshake(e) => write!(f, "握手被拒：{e}"),
            WsError::Transport(e) => write!(f, "连接异常：{e}"),
        }
    }
}

/// 重连退避：第 `attempt` 次失败后等多久（`attempt` 从 1 开始）→ 1/2/4/8/16/32/64/64…
///
/// 与 `bili-danmu` 的退避同形（差别只是封顶 30s → 64s：抖音握手额外要走一轮签名，
/// 打得更稀疏些更省事）。
pub fn reconnect_delay(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.saturating_sub(1).min(6)).min(RECONNECT_MAX_BACKOFF_SECS))
}

/// 拼 WS URL。参数与顺序照实测脚本 `probe/node/ws-probe.cjs::buildUrl`（已实测可握手），
/// 不要重排、不要删——`app_name`/`version_code`/`internal_ext` 里的值都参与服务端校验。
pub fn build_ws_url(room_id: &str, user_unique_id: &str) -> String {
    // internal_ext 含 | 与 :，必须转义，否则 URI 根本解析不出来
    let internal_ext = sign::encode_component(&format!(
        "internal_src:dim|wss_push_room_id:{room_id}|wss_push_did:{user_unique_id}\
         |first_req_ms:0|fetch_time:0|seq:1|wss_info:0-0-0-0|wrds_v:0"
    ));
    format!(
        "{WS_HOST}?app_name=douyin_web&version_code=180800&webcast_sdk_version=1.3.0\
         &update_version_code=1.3.0&compress=gzip&live_id=1&aid=6383&did_rule=3\
         &device_platform=web&identity=audience&room_id={room_id}\
         &user_unique_id={user_unique_id}&cursor=d-1_u-1&host=https://live.douyin.com\
         &im_path=/webcast/im/fetch/&need_persist_msg_count=15&support_wrds=1\
         &internal_ext={internal_ext}"
    )
}

/// 生成 audience 的 `user_unique_id`：19 位数字，形同抖音用户 id。
///
/// 探针用「1 位随机 + 13 位毫秒 + 5 位随机」拼，服务端接受；这里不引随机数依赖，
/// 用毫秒 + 纳秒低位凑后 5 位（同一毫秒内也不会撞）。
pub fn generate_user_unique_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "1{:013}{:05}",
        now.as_millis() as u64,
        now.subsec_nanos() % 100_000
    )
}

/// 单次弹幕会话：签名 → 升级 WS → 收帧 → 回 ack → 心跳，直到断开或被取消。
///
/// 正常结束（被取消）返回 `Ok(())`，其余都是 [`WsError`]，由调用方决定是否重连。
pub async fn run_ws_session(
    cfg: &SessionConfig<'_>,
    signer: &dyn Signer,
    handler: &dyn SessionHandler,
    cancel: CancellationToken,
) -> Result<(), WsError> {
    // 1. 签名：13 个参数按实测顺序拼串 → md5（= X-MS-STUB）→ webmssdk → X-Bogus
    let unsigned = build_ws_url(cfg.room_id, cfg.user_unique_id);
    let md5 = sign::x_ms_stub(&sign::sign_input(&unsigned));
    let started = std::time::Instant::now();
    let bogus = signer
        .sign(md5)
        .await
        .map_err(|e| WsError::Sign(e.to_string()))?;
    if bogus.is_empty() {
        return Err(WsError::Sign("签名器返回了空 X-Bogus".into()));
    }
    log::info!(
        "[douyin] 房间 {} 签名完成（{} ms）",
        cfg.room_id,
        started.elapsed().as_millis()
    );
    let url = format!("{unsigned}&signature={}", sign::encode_component(&bogus));

    // 2. 升级 WS。请求头要与浏览器一致，cookie 里的 ttwid 是握手的必要条件
    let mut req = url
        .as_str()
        .into_client_request()
        .map_err(|e| WsError::Handshake(format!("构造握手请求失败：{e}")))?;
    {
        let headers = req.headers_mut();
        headers.insert("user-agent", HeaderValue::from_static(UA));
        headers.insert("origin", HeaderValue::from_static(ORIGIN));
        headers.insert("accept-language", HeaderValue::from_static("zh-CN,zh;q=0.9"));
        headers.insert(
            "cookie",
            HeaderValue::from_str(cfg.cookie)
                .map_err(|_| WsError::Handshake("cookie 含非法请求头字符".into()))?,
        );
    }
    let connected = tokio::time::timeout(
        Duration::from_secs(CONNECT_TIMEOUT_SECS),
        tokio_tungstenite::connect_async(req),
    )
    .await
    .map_err(|_| WsError::Transport(format!("建连超时 {CONNECT_TIMEOUT_SECS}s")))?
    .map_err(|e| match e {
        // 实测：签名被拒时服务端回 HTTP 200 + Handshake-Msg: auth failed
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            let status = resp.status().as_u16();
            let msg = resp
                .headers()
                .get("handshake-msg")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("（无 Handshake-Msg）");
            WsError::Handshake(format!("HTTP {status} {msg}"))
        }
        other => WsError::Transport(other.to_string()),
    })?;
    let (mut ws, _resp) = connected;
    handler.on_connected();
    log::info!("[douyin] 房间 {} 弹幕会话已建立", cfg.room_id);

    // 3. 心跳 + 收帧
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await; // 跳过首次立即触发

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = heartbeat.tick() => {
                ws.send(Message::Binary(proto::build_heartbeat().into()))
                    .await
                    .map_err(|e| WsError::Transport(format!("发送心跳失败：{e}")))?;
            }
            msg = ws.next() => match msg {
                Some(Ok(Message::Binary(data))) => {
                    if let Some(ack) = handle_frame(&data, handler) {
                        ws.send(Message::Binary(ack.into()))
                            .await
                            .map_err(|e| WsError::Transport(format!("回 ack 失败：{e}")))?;
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    if let Some(ack) = handle_frame(text.as_bytes(), handler) {
                        ws.send(Message::Binary(ack.into()))
                            .await
                            .map_err(|e| WsError::Transport(format!("回 ack 失败：{e}")))?;
                    }
                }
                Some(Ok(_)) => { /* Ping/Pong（tungstenite 自动回 Pong）与其它帧忽略 */ }
                Some(Err(e)) => return Err(WsError::Transport(format!("读帧失败：{e}"))),
                None => return Err(WsError::Transport("连接已被服务端关闭".into())),
            }
        }
    }
}

/// 处理一帧：解外层 `PushFrame` → 解 `Response` → 逐条 parse 交给 handler →
/// 需要 ack 时返回 ack 帧字节（由调用方发出去）。
///
/// **解不开的帧只记日志、不中断会话**：抖音会夹带个别格式异常的帧，
/// 丢一帧的代价远小于整条重连（重连还得多跑一轮签名）。
fn handle_frame(data: &[u8], handler: &dyn SessionHandler) -> Option<Vec<u8>> {
    let frame = match proto::decode_push_frame(data) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("[douyin] 丢弃解不开的帧（{} 字节）：{e}", data.len());
            return None;
        }
    };
    if frame.payload.is_empty() {
        return None; // 心跳回包
    }
    let resp = match proto::decode_response(&frame.payload) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[douyin] 丢弃解不开的 Response（logId={}）：{e}", frame.log_id);
            return None;
        }
    };
    for (method, payload) in &resp.messages {
        if let Some(event) = parser::parse(method, payload) {
            handler.on_event(event);
        }
    }
    // 实测：不回 ack 服务端每 1–2 秒重推同一帧且 cursor 不推进
    if resp.need_ack {
        return Some(proto::build_ack(frame.log_id, &resp.internal_ext));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::douyin::proto::{write_bytes_field, write_str_field, write_varint_field};
    use std::sync::Mutex;

    /// 记录事件的桩 handler
    #[derive(Default)]
    struct Recorder {
        events: Mutex<Vec<DouyinEvent>>,
        connects: Mutex<u32>,
    }

    impl SessionHandler for Recorder {
        fn on_connected(&self) {
            *self.connects.lock().unwrap() += 1;
        }
        fn on_event(&self, event: DouyinEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn v(num: u32, val: u64) -> Vec<u8> {
        let mut o = Vec::new();
        write_varint_field(&mut o, num, val);
        o
    }

    fn s(num: u32, val: &str) -> Vec<u8> {
        let mut o = Vec::new();
        write_str_field(&mut o, num, val);
        o
    }

    fn nested(num: u32, body: &[u8]) -> Vec<u8> {
        let mut o = Vec::new();
        write_bytes_field(&mut o, num, body);
        o
    }

    /// 造一帧：PushFrame{f2=logId, f8=gzip(Response{f1=Message{f1=method,f2=payload}, f5=ext, f9=1})}
    fn frame_with(log_id: u64, method: &str, payload: &[u8]) -> Vec<u8> {
        let mut message = s(1, method);
        write_bytes_field(&mut message, 2, payload);
        let mut resp = Vec::new();
        write_bytes_field(&mut resp, 1, &message);
        write_str_field(&mut resp, 5, "internal_src:pushserver|seq:1");
        write_varint_field(&mut resp, 9, 1);

        let mut frame = v(2, log_id);
        write_bytes_field(&mut frame, 8, &proto::gzip(&resp).unwrap());
        frame
    }

    /// ChatMessage：f1=Common, f2=User, f3=content
    fn chat_payload(content: &str) -> Vec<u8> {
        let common = [v(2, 999), v(4, 1_700_000_000_000)].concat();
        let user = [v(1, 42), s(3, "张三")].concat();
        [nested(1, &common), nested(2, &user), s(3, content)].concat()
    }

    #[test]
    fn 一帧能解出弹幕并回_ack() {
        let rec = Recorder::default();
        let frame = frame_with(424242, "WebcastChatMessage", &chat_payload("好听"));
        let ack = handle_frame(&frame, &rec).expect("带 need_ack 的帧应产出 ack");

        let events = rec.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        let DouyinEvent::Danmaku(d) = &events[0] else {
            panic!("应解出弹幕");
        };
        assert_eq!(d.username, "张三");
        assert_eq!(d.content, "好听");

        // ack 必须带这一帧的 logId 与 internal_ext，否则服务端继续重推
        let expected = proto::build_ack(424242, "internal_src:pushserver|seq:1");
        assert_eq!(ack, expected);
    }

    #[test]
    fn 解不开的帧只丢帧不报错() {
        let rec = Recorder::default();
        assert!(handle_frame(b"\xff\xff\xff\xff", &rec).is_none());
        assert!(rec.events.lock().unwrap().is_empty());
    }

    #[test]
    fn 空_payload_的心跳回包不产生事件也不回_ack() {
        let rec = Recorder::default();
        let hb = proto::build_heartbeat();
        assert!(handle_frame(&hb, &rec).is_none());
        assert!(rec.events.lock().unwrap().is_empty());
    }

    #[test]
    fn 不关心的消息类型不入事件流() {
        let rec = Recorder::default();
        // WebcastInRoomBannerMessage 实测量极大，故意不解析
        let frame = frame_with(1, "WebcastInRoomBannerMessage", b"\x08\x01");
        assert!(handle_frame(&frame, &rec).is_some(), "仍要回 ack");
        assert!(rec.events.lock().unwrap().is_empty());
    }

    #[test]
    fn 重连退避序列() {
        let secs: Vec<u64> = (1..=9).map(|a| reconnect_delay(a).as_secs()).collect();
        assert_eq!(secs, vec![1, 2, 4, 8, 16, 32, 64, 64, 64]);
    }

    #[test]
    fn 签名失败不重试其余退避重连() {
        let sign = WsError::Sign("webmssdk 未就绪".into());
        assert!(!sign.retryable(), "签名失败重试只会把同一次失败再跑一遍");
        assert!(WsError::Handshake("HTTP 200 auth failed".into()).retryable());
        assert!(WsError::Transport("读帧失败".into()).retryable());
    }

    #[test]
    fn ws_url_与实测脚本一致() {
        let url = build_ws_url("7686161414943623974", "1700000000000000000");
        assert!(url.starts_with("wss://webcast3-ws-web-lf.douyin.com/webcast/im/push/v2/?"));
        for part in [
            "app_name=douyin_web",
            "version_code=180800",
            "webcast_sdk_version=1.3.0",
            "compress=gzip",
            "room_id=7686161414943623974",
            "user_unique_id=1700000000000000000",
            "cursor=d-1_u-1",
            "host=https://live.douyin.com",
            "im_path=/webcast/im/fetch/",
            "identity=audience",
        ] {
            assert!(url.contains(part), "URL 缺少实测参数 {part}");
        }
        // internal_ext 必须转义：| → %7C、: → %3A
        assert!(url.contains("&internal_ext=internal_src%3Adim%7Cwss_push_room_id%3A"));
        assert!(!url.contains('|'), "URL 里不能出现未转义的 |（http crate 会拒）");
    }

    #[test]
    fn user_unique_id_是_19_位数字() {
        let uid = generate_user_unique_id();
        assert_eq!(uid.len(), 19, "{uid}");
        assert!(uid.chars().all(|c| c.is_ascii_digit()), "{uid}");
    }
}
