//! 礼物朗读：连击聚合与静默窗口判定
//!
//! 与 `gift` 模块（礼物**列表**渲染）刻意各算一遍，不共用状态：
//! 渲染侧只在过渲染门槛时才产出累计行，而朗读门槛与它独立（可以更低甚至不同开关），
//! 拿不到「未过渲染门槛的累计金额」，所以朗读自己按「用户 + 礼物」分组。
//!
//! 时机：连击窗口内不响（否则连送 20 个就念 20 遍），窗口结束后念一次汇总。
//! 数量与金额取服务端给的**最新累计值**（抖音的 `combo_count` 是连击累计数，
//! 不是增量），行内不做本地累加。

use crate::config::GiftTtsConfig;
use crate::douyin::event::Backing;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 进行中的连击分组
struct Pending {
    /// 最近一次收到的行（数量与金额都是服务端给的累计值）
    row: Backing,
    /// 最近一次收到该分组事件的时间（静默窗口的起点）
    last_at: Instant,
}

/// 礼物朗读聚合器：纯数据 + 纯逻辑，不碰队列与定时器，便于单测
#[derive(Default)]
pub(super) struct GiftAggregator {
    groups: HashMap<String, Pending>,
}

impl GiftAggregator {
    /// 收一条打赏：按「用户 + 礼物」分组，刷新静默窗口起点，永不立即朗读
    /// （抖音只有礼物一种打赏形态，没有需要即时开念的醒目留言 / 上舰）
    pub(super) fn on_backing(&mut self, b: Backing, now: Instant) {
        let key = format!("{}-{}", b.uid, b.gift_id);
        match self.groups.get_mut(&key) {
            Some(p) => {
                p.last_at = now;
                p.row.num = b.num;
                p.row.amount_fen = b.amount_fen;
            }
            None => {
                self.groups.insert(
                    key,
                    Pending {
                        row: b,
                        last_at: now,
                    },
                );
            }
        }
    }

    /// 取出静默已满一个窗口的分组（连击结束），默认门槛没过的直接丢弃
    pub(super) fn take_due(
        &mut self,
        now: Instant,
        window: Duration,
        cfg: &GiftTtsConfig,
    ) -> Vec<Backing> {
        let threshold = threshold_fen(cfg);
        let mut due = Vec::new();
        self.groups.retain(|_, p| {
            if now.duration_since(p.last_at) < window {
                return true;
            }
            if p.row.amount_fen >= threshold {
                due.push(p.row.clone());
            }
            false
        });
        due
    }

    /// 清空未朗读的分组（关闭礼物朗读时调用）
    pub(super) fn clear(&mut self) {
        self.groups.clear();
    }
}

/// 朗读金额门槛 → 分（门槛以人民币元配置）
pub(super) fn threshold_fen(cfg: &GiftTtsConfig) -> u64 {
    (cfg.min_amount_yuan.max(0.0) * 100.0).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::douyin::event::BackingKind;

    fn backing(uid: i64, gift_id: i64, amount_fen: u64, num: u32) -> Backing {
        Backing {
            id: format!("{uid}-{gift_id}-{amount_fen}"),
            kind: BackingKind::Gift,
            uid,
            username: "老板A".into(),
            gift_name: "辣条".into(),
            gift_id,
            num,
            amount_fen,
            timestamp: 1000,
            level: None,
        }
    }

    fn cfg() -> GiftTtsConfig {
        GiftTtsConfig::default()
    }

    fn window() -> Duration {
        Duration::from_secs(5)
    }

    #[test]
    fn 连击在窗口内不朗读窗口结束后汇总一次() {
        let mut agg = GiftAggregator::default();
        let now = Instant::now();
        // 连送三次（服务端每次给累计数）：一次都不该立刻出声
        for n in 1..=3u32 {
            agg.on_backing(backing(1, 100, 100 * n as u64, n), now);
        }

        // 窗口未满 → 还不念
        assert!(agg
            .take_due(now + Duration::from_secs(4), window(), &cfg())
            .is_empty());
        // 窗口满了 → 念一次，数量与金额都是累计值
        let due = agg.take_due(now + window(), window(), &cfg());
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].num, 3);
        assert_eq!(due[0].amount_fen, 300);
        // 取走后不再重复
        assert!(agg
            .take_due(now + window(), window(), &cfg())
            .is_empty());
    }

    #[test]
    fn 同一观众不同礼物各自成组() {
        let mut agg = GiftAggregator::default();
        let now = Instant::now();
        agg.on_backing(backing(1, 100, 100, 1), now);
        agg.on_backing(backing(1, 200, 500, 1), now);
        agg.on_backing(backing(2, 100, 100, 1), now);
        let due = agg.take_due(now + window(), window(), &cfg());
        assert_eq!(due.len(), 3, "三种「用户 + 礼物」组合应各念一次");
    }

    #[test]
    fn 门槛按累计后的总额判定() {
        let mut cfg = cfg();
        cfg.min_amount_yuan = 30.0; // 3000 分

        // 连送累计 2900 分未过门槛，静默后直接丢弃
        let mut agg = GiftAggregator::default();
        let now = Instant::now();
        agg.on_backing(backing(1, 100, 2900, 29), now);
        assert!(agg.take_due(now + window(), window(), &cfg).is_empty());

        // 累计到 3000 分应过门槛
        let mut agg = GiftAggregator::default();
        agg.on_backing(backing(1, 100, 3000, 30), now);
        let due = agg.take_due(now + window(), window(), &cfg);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].amount_fen, 3000);
        assert_eq!(due[0].num, 30);
    }

    #[test]
    fn 窗口内再次送达会延长静默判定() {
        let mut agg = GiftAggregator::default();
        let now = Instant::now();
        agg.on_backing(backing(1, 100, 100, 1), now);
        // 第 4 秒又来一个：静默重新计时，第 5 秒还不该念
        agg.on_backing(backing(1, 100, 200, 2), now + Duration::from_secs(4));
        assert!(agg.take_due(now + window(), window(), &cfg()).is_empty());
        let due = agg.take_due(now + Duration::from_secs(9), window(), &cfg());
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].num, 2);
    }

    #[test]
    fn 清空后不再朗读旧分组() {
        let mut agg = GiftAggregator::default();
        let now = Instant::now();
        agg.on_backing(backing(1, 100, 100, 1), now);
        agg.clear();
        assert!(agg.take_due(now + window(), window(), &cfg()).is_empty());
    }
}
