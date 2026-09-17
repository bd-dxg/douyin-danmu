//! 礼物列表：金额门槛与连击合并
//!
//! 打赏事件从 WS 层进来（`on_backing`），在这里判定后广播 `gift` 事件给弹幕窗：
//! 1. 连击合并：同一观众 + 同一种礼物在 `combo_window_secs` 内合并成同一行；
//!    行的 id 稳定，前端按 id 覆盖更新而不是新增行
//! 2. 金额门槛：按合并后的总额判定，没到门槛的连击先在分组里攒着，攒够才出现
//!
//! **数量与金额都取最新值，不在本地累加**：抖音下发的是连击**累计数**
//! （`GiftMessage.combo_count`，参考实现 `probe/reference/liveMan.py` 直接拿它当数量），
//! 所以一条消息就带着「目前为止送了多少」。B 站那边 `SEND_GIFT` 给的是增量、
//! 另有 `COMBO_SEND` 只用来延长窗口，两边语义不同，别把 B 站那套累加搬回来
//! ——那会把连击数变成平方。

use crate::config::GiftConfig;
use crate::douyin::event::Backing;
use crate::state::OverlayState;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// 广播节流窗口：同一窗口内的更新合并成一次 IPC，避免连击时每个礼物都发一条事件。
/// 最后一次状态不会丢：每次更新都写进 pending，由延迟任务统一冲出去。
const EMIT_INTERVAL: Duration = Duration::from_millis(100);

/// 进行中的连击分组
struct Group {
    /// 广播给前端的行（id 稳定，前端按它覆盖更新）
    row: Backing,
    /// 最近一次收到该分组事件的时间（窗口判定）
    last_at: Instant,
}

#[derive(Default)]
struct Inner {
    /// 进行中的连击分组，键 = 「用户 + 礼物」
    groups: HashMap<String, Group>,
    /// 待广播的行，按行 id 去重（连击只留最新一次）
    pending: HashMap<String, Backing>,
    /// 是否已有冲刷任务在等（避免每条事件都 spawn 一个）
    flushing: bool,
}

/// 礼物列表运行态（Tauri 托管）
#[derive(Default)]
pub(crate) struct GiftState {
    inner: Mutex<Inner>,
}

/// 清空连击分组与待广播队列（断开连接时调用，避免换房后旧分组把新礼物并进旧行）
pub(crate) fn clear(app: &AppHandle) {
    let st = app.state::<GiftState>();
    let mut inner = st.inner.lock().unwrap();
    inner.groups.clear();
    inner.pending.clear();
}

/// 处理一条打赏事件 → 合并 / 过门槛 → 进待广播队列
pub(crate) fn on_backing(app: &AppHandle, b: Backing) {
    let cfg = app.state::<OverlayState>().gift.lock().unwrap().clone();
    if !cfg.enabled {
        return;
    }
    // 锁内只算不广播：emit 是跨 webview 的分发，压在状态锁里迟早把锁序搅乱
    let need_flush = {
        let st = app.state::<GiftState>();
        let mut inner = st.inner.lock().unwrap();
        match merge(&mut inner, b, Instant::now(), &cfg) {
            Some(row) => {
                inner.pending.insert(row.id.clone(), row);
                !std::mem::replace(&mut inner.flushing, true)
            }
            None => false,
        }
    };
    if need_flush {
        spawn_flush(app.clone());
    }
}

/// 等一个节流窗口再冲刷 pending：连击的最后一次数量必定发出
fn spawn_flush(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(EMIT_INTERVAL).await;
        let st = app.state::<GiftState>();
        let rows = {
            let mut inner = st.inner.lock().unwrap();
            take_pending(&mut inner)
        };
        for row in rows {
            let _ = app.emit("gift", &row);
        }
    });
}

/// 取出待广播的行并复位冲刷标记（锁内调用；标记先清再取，
/// 保证取完之后到达的事件能重新排一次冲刷，不会把更新卡在 pending 里）
fn take_pending(inner: &mut Inner) -> Vec<Backing> {
    inner.flushing = false;
    inner.pending.drain().map(|(_, row)| row).collect()
}

/// 把一条打赏并入分组表，返回应当广播的行（未过门槛返回 None）
fn merge(inner: &mut Inner, b: Backing, now: Instant, cfg: &GiftConfig) -> Option<Backing> {
    let window = Duration::from_secs(cfg.combo_window_secs.max(1));
    // 窗口外的分组视为已结束：其行留在前端列表里不再变化
    inner
        .groups
        .retain(|_, g| now.duration_since(g.last_at) < window);

    // 连击分组键 = 用户 + 礼物（窗口结束后同一观众再送同一种礼物会拿到新行）
    let key = format!("{}-{}", b.uid, b.gift_id);
    let row = match inner.groups.get_mut(&key) {
        Some(g) => {
            g.last_at = now;
            // 覆盖而非累加：服务端给的就是累计值（见文件头注释）
            g.row.num = b.num;
            g.row.amount_fen = b.amount_fen;
            g.row.timestamp = b.timestamp;
            g.row.clone()
        }
        None => {
            let mut row = b;
            // 行 id = 分组键 + 首次时间戳
            row.id = format!("{key}-{}", row.timestamp);
            inner.groups.insert(
                key,
                Group {
                    row: row.clone(),
                    last_at: now,
                },
            );
            row
        }
    };

    if row.amount_fen < threshold_fen(cfg) {
        return None;
    }
    Some(row)
}

/// 金额门槛 → 分（门槛以人民币元配置）
fn threshold_fen(cfg: &GiftConfig) -> u64 {
    (cfg.min_amount_yuan.max(0.0) * 100.0).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::douyin::event::BackingKind;

    /// 构造一条礼物打赏（`num` 是服务端给的**累计**连击数，金额已含它）
    fn gift(uid: i64, gift_id: i64, amount_fen: u64, num: u32, ts: i64) -> Backing {
        Backing {
            id: format!("gift-{uid}-{gift_id}-{ts}"),
            kind: BackingKind::Gift,
            uid,
            username: "老板A".into(),
            gift_name: "辣条".into(),
            gift_id,
            num,
            amount_fen,
            timestamp: ts,
            level: None,
        }
    }

    /// 默认门槛 0 元、窗口 5 秒
    fn cfg() -> GiftConfig {
        GiftConfig::default()
    }

    #[test]
    fn 窗口内合并取最新累计值而不是累加() {
        let mut inner = Inner::default();
        let now = Instant::now();
        let first = merge(&mut inner, gift(1, 100, 100, 1, 1000), now, &cfg()).unwrap();
        // 服务端第 2 条给的是累计 2 个 / 200 分
        let second = merge(
            &mut inner,
            gift(1, 100, 200, 2, 1001),
            now + Duration::from_secs(1),
            &cfg(),
        )
        .unwrap();
        assert_eq!(first.id, second.id, "同一连击分组应复用同一行 id");
        assert_eq!(second.num, 2, "累计值应覆盖，不是 1+2=3");
        assert_eq!(second.amount_fen, 200, "金额同理，累加会翻成 300");
    }

    #[test]
    fn 窗口外不合并() {
        let mut inner = Inner::default();
        let now = Instant::now();
        let first = merge(&mut inner, gift(1, 100, 100, 1, 1000), now, &cfg()).unwrap();
        let later = merge(
            &mut inner,
            gift(1, 100, 100, 1, 1100),
            now + Duration::from_secs(6),
            &cfg(),
        )
        .unwrap();
        assert_ne!(first.id, later.id, "窗口外的再次送出应是新的一行");
        assert_eq!(later.num, 1);
    }

    #[test]
    fn 不同礼物或不同用户不合并() {
        let mut inner = Inner::default();
        let now = Instant::now();
        let a = merge(&mut inner, gift(1, 100, 100, 1, 1000), now, &cfg()).unwrap();
        let b = merge(
            &mut inner,
            gift(1, 200, 100, 1, 1000),
            now + Duration::from_secs(1),
            &cfg(),
        )
        .unwrap();
        let c = merge(
            &mut inner,
            gift(2, 100, 100, 1, 1000),
            now + Duration::from_secs(2),
            &cfg(),
        )
        .unwrap();
        assert_ne!(a.id, b.id);
        assert_ne!(a.id, c.id);
        assert_eq!(c.num, 1);
    }

    #[test]
    fn 未过门槛不广播攒够后广播() {
        let mut inner = Inner::default();
        let now = Instant::now();
        let mut cfg = cfg();
        cfg.min_amount_yuan = 30.0; // 3000 分
        assert!(merge(&mut inner, gift(1, 100, 1000, 10, 1000), now, &cfg).is_none());
        assert!(
            merge(
                &mut inner,
                gift(1, 100, 2000, 20, 1001),
                now + Duration::from_secs(1),
                &cfg
            )
            .is_none(),
            "累计到 2000 分仍未过门槛"
        );
        let row = merge(
            &mut inner,
            gift(1, 100, 3000, 30, 1002),
            now + Duration::from_secs(2),
            &cfg,
        )
        .expect("累计到 3000 分应过门槛");
        assert_eq!(row.amount_fen, 3000);
        assert_eq!(row.num, 30);
    }

    #[test]
    fn 门槛为_0_时第一笔就广播() {
        let mut inner = Inner::default();
        let mut cfg = cfg();
        cfg.min_amount_yuan = 0.0;
        assert!(merge(&mut inner, gift(1, 100, 1, 1, 1000), Instant::now(), &cfg).is_some());
    }

    #[test]
    fn 待广播队列按行去重后再取() {
        let mut inner = Inner::default();
        // 同一行连击三次：pending 里只应留一份（最新数量）
        for n in 1..=3u32 {
            let mut row = gift(1, 100, 100 * n as u64, n, 1000 + n as i64);
            row.id = "row-1".into();
            inner.pending.insert(row.id.clone(), row);
            inner.flushing = true;
        }
        let rows = take_pending(&mut inner);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].num, 3);
        assert!(!inner.flushing, "取完要复位标记，后续事件才能再排一次冲刷");
        assert!(inner.pending.is_empty());
    }

    #[test]
    fn 断开连接后分组被清空() {
        let mut inner = Inner::default();
        let now = Instant::now();
        merge(&mut inner, gift(1, 100, 100, 1, 1000), now, &cfg()).unwrap();
        inner.groups.clear();
        let next = merge(
            &mut inner,
            gift(1, 100, 100, 1, 1001),
            now + Duration::from_secs(1),
            &cfg(),
        )
        .unwrap();
        assert_eq!(next.num, 1, "清空后不应把新礼物并进旧行");
    }
}
