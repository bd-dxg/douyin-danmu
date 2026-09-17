//! 抖音消息解析：`(method, payload)` → `DouyinEvent`
//!
//! 字段号见 `probe/reference/douyin.proto` 与 `probe/README.md`（均为实测）。

use super::event::{Backing, BackingKind, Danmaku, DouyinEvent, Welcome, WelcomeKind};
use super::proto::{self, read_fields, Field};

/// 1 钻石 = 多少分（人民币）。
///
/// 抖音充值口径 1 元 = 10 抖币 = 10 钻石，即 1 钻石 = 0.1 元 = 10 分。
/// **这个换算没有核对过实价**（探针里 小心心/粉丝团灯牌 都读到 diamondCount=1），
/// 待与真实账单核对后再定。
pub const DIAMOND_FEN: u64 = 10;

/// 关注的 `SocialMessage`：没有区分关注/分享的可靠字段，v1 一律按「关注」处理。
/// 实测样本 `WebcastSocialMessage fields=1,2,4,5,6,7`（含 shareTarget 与 followCount），
/// action(f4) 的取值语义未实测 → 不做细分。

/// `Common` 里我们要的两个值
struct Common {
    msg_id: u64,
    ts: i64,
}

/// 从 payload 里取 `Common`（f1）。消息体没有 Common 时用 0 + 当前时间兜底。
fn common(fields: &[Field<'_>], fallback_ts: i64) -> Common {
    let Some(raw) = proto::message(fields, 1) else {
        return Common { msg_id: 0, ts: fallback_ts };
    };
    let Ok(cf) = read_fields(raw) else {
        return Common { msg_id: 0, ts: fallback_ts };
    };
    let msg_id = proto::varint(&cf, 2).unwrap_or(0);
    // f4 = createTime（毫秒）；为 0 时说明这条消息没带时间
    let ms = proto::varint(&cf, 4).unwrap_or(0);
    let ts = if ms > 0 { (ms / 1000) as i64 } else { fallback_ts };
    Common { msg_id, ts }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 用户的三元组：id / 昵称 / 身份徽章（等级 + 灯牌）
struct UserInfo {
    uid: i64,
    nick: String,
    level: Option<u32>,
    /// 粉丝团（灯牌）等级，没加团为 None
    fansclub_level: Option<u32>,
}

/// 解 `User`（f1=id, f3=nickName, f23=PayGrade）
fn user(raw: &[u8]) -> Option<UserInfo> {
    let f = read_fields(raw).ok()?;
    let uid = proto::varint(&f, 1).unwrap_or(0) as i64;
    let nick = proto::string(&f, 3).unwrap_or_default();
    // 用户等级（抖音叫「荣誉等级」）在 PayGrade(f23) 里：先读它的 f6 varint，
    // 读不到就从等级图标的 URL 抠（图标名 `new_user_grade_level_v1_N.png`）。
    // **User.Level(f6) 不能用**：真机看到的那个值是该用户在本直播间的粉丝团（灯牌）等级，
    // 当用户等级显示会错；且没有灯牌的观众根本不带这个字段（本机已实测）。
    let level = proto::message(&f, 23)
        .and_then(pay_grade_level)
        .or_else(|| find_grade_level_at(raw, 0));
    // 粉丝团（灯牌）：`User.f24 = FansClub{f1 = FansClubData{f2 = level}}`（实测房间 731918444565）
    let fansclub_level = proto::message(&f, 24).and_then(fansclub_level);
    if uid == 0 && nick.is_empty() {
        return None;
    }
    Some(UserInfo {
        uid,
        nick,
        level,
        fansclub_level,
    })
}

/// 粉丝团（灯牌）等级：`User.f24 = FansClub{f1 = FansClubData{f2 = level}}`
///
/// 实测：加入了本直播间粉丝团的观众才有值（没加团的 f24 是个十字节空壳）。
/// 灯牌名拿不到（`FansClubData.clubName` 为空），所以只取等级。
fn fansclub_level(fans_club: &[u8]) -> Option<u32> {
    let outer = read_fields(fans_club).ok()?;
    let data = read_fields(proto::message(&outer, 1)?).ok()?;
    proto::varint(&data, 2)
        .map(|v| v as u32)
        .filter(|v| *v > 0)
}

/// PayGrade 里的等级：varint(f6) 优先，没值再从等级图标 URL 抠
fn pay_grade_level(pay_grade: &[u8]) -> Option<u32> {
    let f = read_fields(pay_grade).ok()?;
    proto::varint(&f, 6)
        .map(|v| v as u32)
        .filter(|v| *v > 0)
        .or_else(|| find_grade_level_at(pay_grade, 0))
}

/// 在整个 `User` 里挖等级图标的 URL，把等级数字抠出来
///
/// 图标 URL 形如 `https://p11-webcast.douyinpic.com/img/webcast/new_user_grade_level_v1_21.png~tplv-...`
/// （等级在版本号 `v1` 之后），与 DanmuFree 的 `grade_level_v\d+_(\d+)` 同一做法。
/// 不知道 URL 埋在第几层（实测 f21 徽章列表、f23.f19、f61 都挂过同一张图），所以逐层递归：
/// 只认能匹配上的字符串，不会误伤其它字段。
fn find_grade_level_at(bytes: &[u8], depth: u32) -> Option<u32> {
    // 抖音这个结构就三五层深，限深防脏数据把递归撑爆
    if depth > 5 {
        return None;
    }
    for f in read_fields(bytes).ok()? {
        if f.wire != proto::WIRE_LEN {
            continue;
        }
        if let Some(n) = f.as_str().and_then(grade_level_from_url) {
            return Some(n);
        }
        if let Some(n) = find_grade_level_at(f.bytes, depth + 1) {
            return Some(n);
        }
    }
    None
}

/// `...grade_level_v<版本>_<等级>.png` → 等级数字
fn grade_level_from_url(s: &str) -> Option<u32> {
    let rest = &s[s.find("grade_level_v")? + "grade_level_v".len()..];
    let after_version = rest.split_once('_')?.1;
    after_version
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// 主入口：把一条 `Message` 解析成事件；不认识或不需要的返回 None
pub fn parse(method: &str, payload: &[u8]) -> Option<DouyinEvent> {
    let ts = now_secs();
    match method {
        "WebcastChatMessage" => parse_chat(payload, ts).map(DouyinEvent::Danmaku),
        "WebcastGiftMessage" => parse_gift(payload, ts).map(DouyinEvent::Backing),
        "WebcastMemberMessage" => parse_member(payload, ts).map(DouyinEvent::Welcome),
        "WebcastSocialMessage" => parse_social(payload, ts).map(DouyinEvent::Welcome),
        "WebcastLikeMessage" => parse_like(payload, ts).map(DouyinEvent::Welcome),
        // 其余（Banner / Rank / RoomStats / GiftSort / RoomUserSeq …）全部忽略
        _ => None,
    }
}

/// `ChatMessage`：f1=Common, f2=User, f3=content
fn parse_chat(payload: &[u8], fallback_ts: i64) -> Option<Danmaku> {
    let f = read_fields(payload).ok()?;
    let c = common(&f, fallback_ts);
    let u = user(proto::message(&f, 2)?)?;
    let content = proto::string(&f, 3).unwrap_or_default();
    if content.trim().is_empty() {
        return None;
    }
    Some(Danmaku {
        id: event_id(c.msg_id, c.ts, u.uid),
        username: u.nick,
        content,
        timestamp: c.ts,
        level: u.level,
        fansclub_level: u.fansclub_level,
    })
}

/// `GiftMessage`：f2=giftId, f5=repeatCount, f6=comboCount, f7=User, f15=GiftStruct(f12=diamondCount, f16=name)
fn parse_gift(payload: &[u8], fallback_ts: i64) -> Option<Backing> {
    let f = read_fields(payload).ok()?;
    let c = common(&f, fallback_ts);
    let u = user(proto::message(&f, 7)?)?;

    let gift = proto::message(&f, 15).map(|raw| read_fields(raw).ok()).flatten()?;
    let gift_name = proto::string(&gift, 16).unwrap_or_default();
    let diamond = proto::varint(&gift, 12).unwrap_or(0);
    let gift_id = proto::varint(&f, 2).unwrap_or(0) as i64;

    let combo = proto::varint(&f, 6).unwrap_or(0);
    let repeat = proto::varint(&f, 5).unwrap_or(0);
    let num = if combo > 0 { combo } else { repeat }.max(1) as u32;

    // 免费礼物（钻石价为 0）不产生打赏事件
    if diamond == 0 || gift_name.is_empty() {
        return None;
    }

    Some(Backing {
        id: event_id(c.msg_id, c.ts, u.uid),
        kind: BackingKind::Gift,
        uid: u.uid,
        username: u.nick,
        gift_name,
        gift_id,
        num,
        amount_fen: diamond.saturating_mul(DIAMOND_FEN).saturating_mul(num as u64),
        timestamp: c.ts,
        level: u.level,
    })
}

/// `MemberMessage`：f2=User
fn parse_member(payload: &[u8], fallback_ts: i64) -> Option<Welcome> {
    let f = read_fields(payload).ok()?;
    let c = common(&f, fallback_ts);
    let u = user(proto::message(&f, 2)?)?;
    Some(Welcome {
        id: event_id(c.msg_id, c.ts, u.uid),
        kind: WelcomeKind::Enter,
        uid: u.uid,
        username: u.nick,
        timestamp: c.ts,
    })
}

/// `SocialMessage`：f2=User
fn parse_social(payload: &[u8], fallback_ts: i64) -> Option<Welcome> {
    let f = read_fields(payload).ok()?;
    let c = common(&f, fallback_ts);
    let u = user(proto::message(&f, 2)?)?;
    Some(Welcome {
        id: event_id(c.msg_id, c.ts, u.uid),
        kind: WelcomeKind::Follow,
        uid: u.uid,
        username: u.nick,
        timestamp: c.ts,
    })
}

/// `LikeMessage`：f2=count, f3=total, f5=User
fn parse_like(payload: &[u8], fallback_ts: i64) -> Option<Welcome> {
    let f = read_fields(payload).ok()?;
    let c = common(&f, fallback_ts);
    let u = user(proto::message(&f, 5)?)?;
    Some(Welcome {
        id: event_id(c.msg_id, c.ts, u.uid),
        kind: WelcomeKind::Like,
        uid: u.uid,
        username: u.nick,
        timestamp: c.ts,
    })
}

/// 事件 id：优先服务端 msgId，缺省时用「时间戳-用户」兜底（与 B 站侧同一策略）
fn event_id(msg_id: u64, ts: i64, uid: i64) -> String {
    if msg_id > 0 {
        msg_id.to_string()
    } else {
        format!("{ts}-{uid}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::douyin::proto::{write_bytes_field, write_str_field, write_varint_field};

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

    /// Common：f2=msgId, f4=createTime(ms)
    fn common_body(msg_id: u64, ms: u64) -> Vec<u8> {
        [v(2, msg_id), v(4, ms)].concat()
    }

    /// User：f1=id, f3=nick, f6=Level（真机实测这个 f6 是**本房间粉丝团等级**，不是用户等级）
    fn user_body(uid: u64, nick: &str, f6: Option<u64>) -> Vec<u8> {
        let mut o = [v(1, uid), s(3, nick)].concat();
        if let Some(l) = f6 {
            o.extend(v(6, l));
        }
        o
    }

    /// User 带上粉丝团（f24 = FansClub{f1 = FansClubData{f2 = level, f3 = status}}）
    fn with_fansclub(mut user: Vec<u8>, level: u64) -> Vec<u8> {
        let data = [v(2, level), v(3, 1)].concat();
        user.extend(nested(24, &nested(1, &data)));
        user
    }

    /// User 带上 PayGrade（用户等级真正的位位置）
    fn user_body_with_grade(uid: u64, nick: &str, grade: u32) -> Vec<u8> {
        let url = format!(
            "https://p3-pc.douyinpic.com/aweme/100x100/new_user_grade_level_v1_{grade}.png~tplv-x.webp"
        );
        // PayGrade{ f4=icon{ f1=url_list } }
        let icon = s(1, &url);
        let pay_grade = nested(4, &icon);
        [user_body(uid, nick, None), nested(23, &pay_grade)].concat()
    }

    /// User 带上 PayGrade 的等级 varint（真机：`f23{f6=1}` 就是荣誉等级 1 级）
    fn user_body_with_grade_varint(uid: u64, nick: &str, grade: u64) -> Vec<u8> {
        [user_body(uid, nick, None), nested(23, &v(6, grade))].concat()
    }

    #[test]
    fn 弹幕解出昵称正文与等级() {
        let payload = [
            nested(1, &common_body(999, 1_700_000_000_000)),
            nested(2, &user_body(42, "张三", Some(16))),
            s(3, "好听"),
        ]
        .concat();

        let Some(DouyinEvent::Danmaku(d)) = parse("WebcastChatMessage", &payload) else {
            panic!("应解出弹幕");
        };
        assert_eq!(d.id, "999");
        assert_eq!(d.username, "张三");
        assert_eq!(d.content, "好听");
        assert_eq!(d.timestamp, 1_700_000_000);
        assert_eq!(
            d.level, None,
            "User.f6 是本房间粉丝团等级（可能非 0），不能当用户等级"
        );
    }

    #[test]
    fn 用户等级优先读_paygrade_的_varint() {
        let payload = [
            nested(1, &common_body(1, 0)),
            nested(2, &user_body_with_grade_varint(42, "老板A", 21)),
            s(3, "冒个泡"),
        ]
        .concat();
        let Some(DouyinEvent::Danmaku(d)) = parse("WebcastChatMessage", &payload) else {
            panic!("应解出弹幕");
        };
        assert_eq!(d.level, Some(21));
    }

    #[test]
    fn 用户等级从_paygrade_里的图标_url_抠出来() {
        // 真机样本里 f19 挂着等级图标；PayGrade 没给 varint 时走这条
        let payload = [
            nested(1, &common_body(1, 0)),
            nested(2, &user_body_with_grade(42, "老板A", 21)),
            s(3, "冒个泡"),
        ]
        .concat();
        let Some(DouyinEvent::Danmaku(d)) = parse("WebcastChatMessage", &payload) else {
            panic!("应解出弹幕");
        };
        assert_eq!(d.level, Some(21));
    }

    #[test]
    fn 没有_paygrade_时从徽章列表的图标里抠等级() {
        // 真机样本：User.f21 徽章列表里也挂着同一张等级图标
        let icon = s(
            1,
            "https://p11-webcast.douyinpic.com/img/webcast/new_user_grade_level_v1_33.png~tplv-x",
        );
        let payload = [
            nested(1, &common_body(1, 0)),
            nested(2, &[user_body(42, "老板A", None), nested(21, &icon)].concat()),
            s(3, "冒个泡"),
        ]
        .concat();
        let Some(DouyinEvent::Danmaku(d)) = parse("WebcastChatMessage", &payload) else {
            panic!("应解出弹幕");
        };
        assert_eq!(d.level, Some(33));
    }

    #[test]
    fn 只有粉丝团等级没有_paygrade_时不显示用户等级() {
        // 没消费过/没等级图标的观众：宁可空着，也不能把 f6 当等级显示
        let payload = [
            nested(1, &common_body(1, 0)),
            nested(2, &user_body(42, "小明", Some(7))),
            s(3, "你好"),
        ]
        .concat();
        let Some(DouyinEvent::Danmaku(d)) = parse("WebcastChatMessage", &payload) else {
            panic!("应解出弹幕");
        };
        assert_eq!(d.level, None);
    }

    #[test]
    fn 粉丝团等级从_f24_里读出来() {
        let payload = [
            nested(1, &common_body(1, 0)),
            nested(2, &with_fansclub(user_body(42, "老板A", None), 4)),
            s(3, "冒个泡"),
        ]
        .concat();
        let Some(DouyinEvent::Danmaku(d)) = parse("WebcastChatMessage", &payload) else {
            panic!("应解出弹幕");
        };
        assert_eq!(d.fansclub_level, Some(4));
    }

    #[test]
    fn 没加粉丝团时没有灯牌等级() {
        // 真机：没加团的 f24 是个十字节空壳（没有 level），不能显示成“灯牌0”
        let data = [v(3, 0)].concat();
        let user = [user_body(42, "新观众", None), nested(24, &nested(1, &data))].concat();
        let payload = [nested(1, &common_body(1, 0)), nested(2, &user), s(3, "第一次来")].concat();
        let Some(DouyinEvent::Danmaku(d)) = parse("WebcastChatMessage", &payload) else {
            panic!("应解出弹幕");
        };
        assert_eq!(d.fansclub_level, None);
        assert_eq!(d.level, None);
    }

    #[test]
    fn 等级_url_解析只认版本号后的数字() {
        assert_eq!(
            grade_level_from_url("https://x/new_user_grade_level_v1_21.png~tplv-y.webp"),
            Some(21)
        );
        assert_eq!(grade_level_from_url("https://x/level_v3_7.png"), None, "别的图标不算");
        assert_eq!(grade_level_from_url("https://x/grade_level_v1_.png"), None);
        assert_eq!(grade_level_from_url(""), None);
    }

    #[test]
    fn 空正文弹幕被丢弃() {
        let payload = [nested(1, &common_body(1, 0)), nested(2, &user_body(1, "A", None)), s(3, "   ")]
            .concat();
        assert!(parse("WebcastChatMessage", &payload).is_none());
    }

    #[test]
    fn 礼物按连击数计价() {
        // GiftStruct: f12=diamondCount, f16=name
        let gift = [v(12, 5), s(16, "玫瑰")].concat();
        let payload = [
            nested(1, &common_body(7, 1_700_000_000_000)),
            v(2, 685),
            v(5, 1), // repeat_count
            v(6, 3), // combo_count
            nested(7, &user_body(8, "老板A", None)),
            nested(15, &gift),
        ]
        .concat();

        let Some(DouyinEvent::Backing(b)) = parse("WebcastGiftMessage", &payload) else {
            panic!("应解出礼物");
        };
        assert_eq!(b.username, "老板A");
        assert_eq!(b.gift_name, "玫瑰");
        assert_eq!(b.gift_id, 685);
        assert_eq!(b.num, 3);
        // 5 钻石 × 10 分 × 3 个
        assert_eq!(b.amount_fen, 150);
    }

    #[test]
    fn 免费礼物不产生打赏事件() {
        let gift = [v(12, 0), s(16, "小心心")].concat();
        let payload = [
            nested(1, &common_body(7, 0)),
            v(6, 1),
            nested(7, &user_body(8, "A", None)),
            nested(15, &gift),
        ]
        .concat();
        assert!(parse("WebcastGiftMessage", &payload).is_none());
    }

    #[test]
    fn 进场关注点赞各归各的形态() {
        let member = [nested(1, &common_body(1, 0)), nested(2, &user_body(1, "B", None))].concat();
        let social = [nested(1, &common_body(2, 0)), nested(2, &user_body(2, "C", None))].concat();
        let like = [
            nested(1, &common_body(3, 0)),
            v(2, 5),
            v(3, 100),
            nested(5, &user_body(3, "D", None)),
        ]
        .concat();

        let kinds = [
            parse("WebcastMemberMessage", &member),
            parse("WebcastSocialMessage", &social),
            parse("WebcastLikeMessage", &like),
        ];
        let kinds: Vec<_> = kinds
            .into_iter()
            .map(|e| match e {
                Some(DouyinEvent::Welcome(w)) => w.kind,
                _ => panic!("应解出欢迎事件"),
            })
            .collect();
        assert_eq!(kinds, vec![WelcomeKind::Enter, WelcomeKind::Follow, WelcomeKind::Like]);
    }

    #[test]
    fn 不关心的消息类型返回_none() {
        assert!(parse("WebcastRoomStatsMessage", b"\x08\x01").is_none());
        assert!(parse("WebcastInRoomBannerMessage", b"") .is_none());
    }

    #[test]
    fn 缺_msgId_时用时间戳与用户兜底() {
        let payload = [
            nested(1, &common_body(0, 1_700_000_000_000)),
            nested(2, &user_body(77, "E", None)),
            s(3, "hi"),
        ]
        .concat();
        let Some(DouyinEvent::Danmaku(d)) = parse("WebcastChatMessage", &payload) else {
            panic!("应解出弹幕");
        };
        assert_eq!(d.id, "1700000000-77");
    }
}
