//! 抖音直播弹幕连接模块
//!
//! 协议细节全部来自实测（探针在仓库根目录 `probe/`，结论见 `probe/README.md`）。
//! 与 B 站的关键差异：
//! - 房间号是 19 位长号（`room_id`），短号（`web_rid`）要先解析
//! - 握手要 `ttwid` cookie + `X-Bogus` 签名（签名要跑 webmssdk 的 JS）
//! - 收到帧后**必须回 ack**，否则服务端反复重推同一帧、cursor 不推进
//! - 礼物消息**只在登录态下才推**（匿名收不到）

pub mod event;
pub mod login;
pub mod parser;
pub mod proto;
pub mod resolver;
pub mod sign;
pub mod signer;
pub mod ws;
