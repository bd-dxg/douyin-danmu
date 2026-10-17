//! 抖音弹幕表情包：表情的识别、以及正文里 `[名字]` 片段的剥离
//!
//! 与 B 站不同，抖音的表情弹幕是**独立的消息类型**（不是 `ChatMessage` 上的一个字段）：
//! 观众在弹幕框点表情发出时，服务端推的就是它，表情名也在这个消息里。
//! 但抖音网页端还会把表情写成 `[名字]` 文本混进**普通弹幕**（`WebcastChatMessage.content`）
//! —— 这类报文没有表情字段，只能按正文形态认，见 [`strip_bracket_emotes`]。
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

/// 一个 `[名字]` 片段里名字的最大字数
///
/// 表情名都很短（实测「发呆」「微笑」），超过这个长度的中括号文本按普通文字处理，
/// 避免把 `[这是一段很长的手打说明]` 剥掉。
const EMOTE_NAME_MAX_CHARS: usize = 30;

/// 剥离正文里的 `[名字]` 表情片段，返回（剩余文字，是否剥到过表情片段）
///
/// 抖音网页端把表情写成 `[名字]` 文本混在**普通弹幕**正文里（实测「[发呆][发呆]」，
/// 这类报文没有表情字段，只有 `WebcastEmojiChatMessage` 才带 `emojiId`），
/// 而且**一条弹幕可能混着文字**（「今天真好[发呆]」）或有多个表情 —— 这类要剥掉表情、
/// 只念剩下的文字。畸形中括号（未闭合、内部还有 `[`、名字为空或过长）一律原样保留，
/// 不得剥坏正文。
///
/// 代价（与 B 站版一致）：用户手打的方括号文字（如「价格[10]元」）也会被当表情剥掉。
pub(crate) fn strip_bracket_emotes(content: &str) -> (String, bool) {
    let chars: Vec<char> = content.chars().collect();
    let mut out = String::with_capacity(content.len());
    let mut had = false;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '[' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // 找最近的 `]`，其间不允许出现 `[`
        let mut j = i + 1;
        while j < chars.len() && chars[j] != ']' && chars[j] != '[' {
            j += 1;
        }
        let well_formed = j < chars.len()
            && chars[j] == ']'
            && j > i + 1
            && j - i - 1 <= EMOTE_NAME_MAX_CHARS;
        if well_formed {
            had = true;
            i = j + 1;
        } else {
            // 畸形：从这里开始整段当普通文字（继续往后找会把 `[a[b]c]` 这类剥坏）
            out.extend(chars[i..].iter());
            break;
        }
    }
    (out, had)
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

    #[test]
    fn 剥离正文里的方括号表情() {
        let cases = [
            ("[发呆]", "", true),
            ("[发呆][发呆]", "", true),
            ("[发呆]今天真好", "今天真好", true),
            ("今天[发呆]真好", "今天真好", true),
            ("今天真好[发呆][发呆]", "今天真好", true),
            // 普通文字一律不动
            ("普通弹幕", "普通弹幕", false),
            ("[前半", "[前半", false),
            ("后半]", "后半]", false),
            ("[a[b]c]", "[a[b]c]", false),
            ("[]", "[]", false),
        ];
        for (input, want, had_want) in cases {
            let (got, had) = strip_bracket_emotes(input);
            assert_eq!(got, want, "「{input}」剥离后应为「{want}」");
            assert_eq!(had, had_want, "「{input}」是否含表情片段");
        }
        // 名字过长（> 30 字）不当表情，避免剥掉长说明文字
        let long = format!("[{}]", "字".repeat(40));
        assert_eq!(strip_bracket_emotes(&long), (long.clone(), false));
    }
}
