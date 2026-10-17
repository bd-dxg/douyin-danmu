//! 抖音弹幕表情包：`WebcastEmojiChatMessage` 的解析
//!
//! 与 B 站不同，抖音的表情弹幕是**独立的消息类型**（不是 `ChatMessage` 上的一个字段）：
//! 观众在弹幕框点表情发出时，服务端推的就是它，表情名也在这个消息里。
//! 字段号见 `probe/reference/douyin.proto`：
//!   f1=Common, f2=User, f3=emojiId, f4=emojiContent(Text), f5=defaultContent, f6=backgroundImage
//!
//! 显示名优先取 f5 `defaultContent`；取不到时退回 f4 `emojiContent` 的 `defaultPattern` / `key`
//! （这两条是兜底，未实测）。都取不到就整条丢掉：正文为空的表情弹幕在弹幕窗里只剩一行空白。

use super::event::Emote;
use super::proto::{self, read_fields, Field};

/// 表情弹幕的解析结果
pub(crate) struct EmoteMessage {
    /// 下发给前端的表情标记（`Danmaku.content` 就是下面的 `name`）
    pub emote: Emote,
    /// 表情显示名（表情弹幕的正文）
    pub name: String,
}

/// 解一条 `WebcastEmojiChatMessage` 的表情
pub(crate) fn parse_emote(fields: &[Field<'_>]) -> Option<EmoteMessage> {
    let name = display_name(fields)?;
    let emoji_id = proto::varint(fields, 3).unwrap_or(0) as i64;
    Some(EmoteMessage {
        emote: Emote { emoji_id },
        name,
    })
}

/// 表情显示名：f5 `defaultContent` 优先，退回 f4 `emojiContent`（`Text`：f1=key, f2=defaultPattern）
fn display_name(fields: &[Field<'_>]) -> Option<String> {
    non_empty(proto::string(fields, 5)).or_else(|| {
        let inner = read_fields(proto::message(fields, 4)?).ok()?;
        non_empty(proto::string(&inner, 2)).or_else(|| non_empty(proto::string(&inner, 1)))
    })
}

/// 去空白后必须非空（空名字等于没名字）
fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
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

    #[test]
    fn 优先取_default_content() {
        let payload = [v(3, 42), s(5, "微笑")].concat();
        let e = parse_emote(&read_fields(&payload).unwrap()).expect("应解出表情");
        assert_eq!(e.name, "微笑");
        assert_eq!(e.emote.emoji_id, 42);
    }

    #[test]
    fn 没有_default_content_时退回_emoji_content() {
        // Text: f1=key, f2=defaultPattern
        let text = [s(1, "[微笑]"), s(2, "微笑")].concat();
        let payload = [v(3, 42), nested(4, &text)].concat();
        let e = parse_emote(&read_fields(&payload).unwrap()).expect("应解出表情");
        assert_eq!(e.name, "微笑", "defaultPattern 优先于 key");
    }

    #[test]
    fn 只有_key_时也用得上() {
        let text = s(1, "[微笑]");
        let payload = [v(3, 42), nested(4, &text)].concat();
        let e = parse_emote(&read_fields(&payload).unwrap()).expect("应解出表情");
        assert_eq!(e.name, "[微笑]");
    }

    #[test]
    fn 名字都取不到时不算表情() {
        // 空 defaultContent + 空 emojiContent：宁可整条丢掉，也不给弹幕窗留一行空白
        assert!(parse_emote(&read_fields(&v(3, 42)).unwrap()).is_none());
        assert!(parse_emote(&read_fields(&s(5, "  ")).unwrap()).is_none());
        let text = s(2, "");
        let payload = [v(3, 42), nested(4, &text)].concat();
        assert!(parse_emote(&read_fields(&payload).unwrap()).is_none());
    }

    #[test]
    fn 缺_emoji_id_时按_0_处理() {
        // 名字在就照常显示，id 只是排障用的
        let e = parse_emote(&read_fields(&s(5, "微笑")).unwrap()).expect("应解出表情");
        assert_eq!(e.emote.emoji_id, 0);
    }
}
