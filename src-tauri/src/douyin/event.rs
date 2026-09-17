//! 抖音直播事件模型
//!
//! 与 B 站模型的差异（决定了字段取舍）：
//! - 抖音没有「舰队」（舰长/提督/总督），也没有房管标记
//! - 粉丝团（灯牌）等级埋在 `User.BadgeImageList` 的嵌套 `content` 里，字段号**未实测**
//!   → v1 不下发（宁愿没有，也不给前端一个永远为空的字段）
//! - 抖音没有醒目留言（SC），打赏只有礼物一种形态
//! - 「用户等级」（抖音叫荣誉等级）在 `User.f23 = PayGrade`，不在 `User.f6`
//!   —— 见 `parser::user()` 的注释与 `Task/findings.md` 第 16 条

use serde::Serialize;

/// 弹幕消息
#[derive(Debug, Clone, Serialize)]
pub struct Danmaku {
    /// 去重标识：优先用服务端 `Common.msgId`
    pub id: String,
    pub username: String,
    pub content: String,
    /// 发送时间（Unix 秒）
    pub timestamp: i64,
    /// 抖音用户等级（荣誉等级）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u32>,
    /// 粉丝团（灯牌）等级：只有已加入本直播间粉丝团的观众才有
    ///
    /// 灯牌**没有名字**（`FansClubData.clubName` 实测为空，网页上那枚灯牌就是一张带等级的图标），
    /// 所以只下发等级。礼物行不传这个字段（打赏行已有用户名与礼物名，再加一枚徽章只挤横向空间）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fansclub_level: Option<u32>,
}

/// 打赏形态。抖音目前只有礼物（无 SC / 无舰队）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackingKind {
    /// 礼物（WebcastGiftMessage）
    Gift,
}

/// 打赏消息（礼物）
///
/// 金额统一折算成整数「分」（`amount_fen`），与 B 站同口径，避免浮点误差。
/// 换算系数见 `crate::douyin::parser::DIAMOND_FEN`（未核对实价，属于待验证项）。
#[derive(Debug, Clone, Serialize)]
pub struct Backing {
    /// 去重标识：优先用服务端 `Common.msgId`
    pub id: String,
    pub kind: BackingKind,
    pub uid: i64,
    pub username: String,
    pub gift_name: String,
    pub gift_id: i64,
    /// 数量：优先连击数（combo_count），为 0 时退回 repeat_count
    pub num: u32,
    /// 人民币价值（分）
    pub amount_fen: u64,
    /// 发送时间（Unix 秒）
    pub timestamp: i64,
    /// 抖音用户等级
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u32>,
}

/// 欢迎事件形态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WelcomeKind {
    /// 进入直播间（WebcastMemberMessage）
    Enter,
    /// 关注（WebcastSocialMessage）
    Follow,
    /// 点赞（WebcastLikeMessage）
    Like,
}

/// 欢迎消息（进房 / 关注 / 点赞）
///
/// 量级比弹幕大（实测进房 : 弹幕 ≈ 2 : 1），单独一路事件：
/// 不进弹幕速率统计，也不参与弹幕筛选，去重与限速由 `crate::welcome` 负责。
#[derive(Debug, Clone, Serialize)]
pub struct Welcome {
    /// 去重标识（优先用服务端 `Common.msgId`）
    pub id: String,
    pub kind: WelcomeKind,
    pub uid: i64,
    pub username: String,
    /// 事件时间（Unix 秒）
    pub timestamp: i64,
}

/// 解析出的抖音事件
///
/// 未列入的命令（礼物榜、在线榜、房间横幅、房间统计等）在 parser 层直接忽略：
/// 抖音这类消息量极大（实测 120 秒内 `WebcastInRoomBannerMessage` 362 条），
/// 全量解析只会造成日志洪泛与无谓分配。
#[derive(Debug, Clone)]
pub enum DouyinEvent {
    Danmaku(Danmaku),
    Backing(Backing),
    Welcome(Welcome),
}
