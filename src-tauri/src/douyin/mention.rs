//! 弹幕里的 @提及：把「@被提及的人 + 说的话」拆开
//!
//! 抖音**没有**独立的「回复」消息类型（探针抓到的 method 里只有 `WebcastChatMessage`），
//! @提及就是正文开头的文本 —— 真机弹幕窗里长这样：`@神秘人六阶 谢谢哥`。
//! 朗读据此念成「某某对神秘人六阶说 谢谢哥」（组装在 `crate::tts::text::build_text`）。
//!
//! 拆解保守：只认正文**开头**的 `@名字`（可在前面留空白），名字与正文之间必须是
//! 空白或不可见字符；名字超过 [`MENTION_NAME_MAX_CHARS`] 字、或 `@` 后面没名字时，
//! 一律按普通弹幕处理（宁可多念一句，也不要把正文当名字念）。

/// 被提及者名字的最大字数（昵称上限：再长就不是昵称，而是把整句正文当成了名字）
const MENTION_NAME_MAX_CHARS: usize = 30;

/// 拆出正文开头的 `@名字`，返回（被提及的名字，剩下的正文）
///
/// 名字取到第一个分隔符为止：昵称里带空格的（「红 桃A」）只会认到第一个空格 ——
/// 认半个名字念出来还能听懂，把整句正文当名字念就完全不成句了。
/// 剩下来的正文可能为空（只 @ 了人、没说话），是否朗读由调用方判定。
pub(crate) fn split_mention(content: &str) -> Option<(String, String)> {
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;
    while i < chars.len() && is_separator(chars[i]) {
        i += 1;
    }
    if !is_at_sign(*chars.get(i)?) {
        return None;
    }
    i += 1;
    let start = i;
    while i < chars.len() && !is_separator(chars[i]) {
        i += 1;
    }
    if i == start || i - start > MENTION_NAME_MAX_CHARS {
        return None;
    }
    let name: String = chars[start..i].iter().collect();
    while i < chars.len() && is_separator(chars[i]) {
        i += 1;
    }
    Some((name, chars[i..].iter().collect()))
}

/// 分隔符：空白（含全角空格、不换行空格）与不可见格式字符
///
/// 抖音把提及的名字与正文用这类字符隔开，网页端肉眼看不见，
/// 不跳过就会把标记一起念出来（或让名字粘连到正文上）。
fn is_separator(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch as u32,
            0x200B..=0x200F // 零宽空格 / 零宽连接符 / LRM / RLM
                | 0x2028..=0x202E // 行分隔符 / 双向控制符
                | 0x2060..=0x2064 // word joiner / 不可见运算符
                | 0x2066..=0x2069 // 双向隔离符
                | 0xFEFF // BOM
        )
}

/// 半角与全角 `@`
fn is_at_sign(ch: char) -> bool {
    ch == '@' || ch == '＠'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(content: &str) -> Option<(String, String)> {
        split_mention(content).map(|(n, rest)| (n, rest.to_string()))
    }

    #[test]
    fn 拆出开头的提及() {
        assert_eq!(split("@神秘人六阶 谢谢哥"), Some(("神秘人六阶".into(), "谢谢哥".into())));
        // 全角 @、多个空格、不可见字符分隔都认
        assert_eq!(split("＠神秘人六阶 谢谢哥"), Some(("神秘人六阶".into(), "谢谢哥".into())));
        assert_eq!(split("@神秘人六阶\t谢谢哥"), Some(("神秘人六阶".into(), "谢谢哥".into())));
        assert_eq!(
            split("@神秘人六阶\u{200E} 谢谢哥"),
            Some(("神秘人六阶".into(), "谢谢哥".into()))
        );
        // 前面留空白不算事；只 @ 了人不说话时正文为空
        assert_eq!(split("  @神秘人六阶"), Some(("神秘人六阶".into(), String::new())));
        // 正文里的表情照样留给下层剥离
        assert_eq!(split("@神秘人六阶 [发呆]"), Some(("神秘人六阶".into(), "[发呆]".into())));
    }

    #[test]
    fn 不在开头的_at_不算提及() {
        assert_eq!(split("谢谢@某人 的礼物"), None);
        assert_eq!(split("恭迎@ ‎乐乐好景分享"), None);
        assert_eq!(split("普通弹幕"), None);
        assert_eq!(split(""), None);
    }

    #[test]
    fn 名字取不到或过长时不算提及() {
        assert_eq!(split("@ 谢谢哥"), None);
        assert_eq!(split("@"), None);
        assert_eq!(split("@谢谢哥"), Some(("谢谢哥".into(), String::new())), "没有分隔符时整段都算名字");
        let long = format!("@{} 你好", "字".repeat(40));
        assert_eq!(split(&long), None);
    }
}
