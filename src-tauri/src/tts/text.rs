//! 朗读文案：筛选判定 + 清洗 + 组装
//!
//! 全文只有纯字符串处理，不碰队列与网络，便于单测覆盖边界。
//! 筛选语义与前端 Overlay 的显示筛选一致（见 `matches_filter` 注释）。

use crate::douyin::event::{Backing, Danmaku};
use crate::config::{DanmakuFilter, TtsConfig};

/// 同一字符连续重复的最大保留次数（「哈哈哈哈哈哈哈」只读三个）
const MAX_REPEAT: u32 = 3;

/// 朗读筛选判定：与前端 Overlay 的显示筛选同语义
/// （身份规则之间是「或」，全部关闭 = 不过滤；敏感词屏蔽独立叠加且不豁免）
pub(super) fn matches_filter(d: &Danmaku, f: &DanmakuFilter) -> bool {
    if f.enable_sensitive {
        let content = d.content.to_lowercase();
        if f.sensitive_words
            .iter()
            .any(|w| !w.is_empty() && content.contains(&w.to_lowercase()))
        {
            return false;
        }
    }
    if !f.enable_level {
        return true;
    }
    d.level.unwrap_or(0) >= f.level_min
}

/// 组装朗读文案：可选用户名 + 清洗后的正文
///
/// 正文没有可读内容（纯 emoji / 纯标点 / 纯链接）时整条返回空，
/// 不能只剩下「小明说」这种没内容的空壳。
/// `max_len` 只约束正文：用户名是固定少量字，不该挤掉弹幕本身。
pub(super) fn build_text(d: &Danmaku, config: &TtsConfig) -> String {
    let mut content = clean_for_speech(&d.content);
    if !has_readable_content(&content) {
        return String::new();
    }
    let max = config.max_len as usize;
    if max > 0 && content.chars().count() > max {
        content = content.chars().take(max).collect();
    }

    let mut out = String::new();
    if config.read_username {
        // 用户名同正文一样要剔噪声字符：昵称里的 `_` 同样会被念成「下划线」
        let name = strip_noise(&d.username);
        let name = name.trim();
        if has_readable_content(name) {
            out.push_str(name);
            out.push_str("说 ");
        }
    }
    out.push_str(&content);
    out.trim().to_string()
}

/// 是否有可读内容：纯标点（`?` `？` `！` `~`）、纯 emoji、纯空白都算「没东西可念」。
/// 汉字与数字在 Unicode 里属于字母/数字，「哈哈哈」「666」都算有内容。
fn has_readable_content(text: &str) -> bool {
    text.chars().any(|ch| ch.is_alphanumeric())
}

/// 组装打赏朗读文案：感谢 + 用户名 + 数量 + 礼物名
///
/// 句式：「感谢老板A送的5个辣条」。礼物名或用户名清洗后为空时降级到不带它的句式，
/// 不出现「感谢送的个」这种断句。
/// 抖音只有礼物一种形态，没有醒目留言与上舰。
pub(super) fn build_backing_text(b: &Backing) -> String {
    let name = spoken_name(&b.username);
    let gift = clean_for_speech(&b.gift_name);
    let num = b.num.max(1);
    if gift.is_empty() {
        format!("感谢{name}送的{num}个礼物")
    } else {
        format!("感谢{name}送的{num}个{gift}")
    }
}

/// 朗读用的用户名：昵称里的 `_` 同样会被念成「下划线」，先剔噪声；
/// 剔完为空（纯符号昵称）时回退「观众」，不能让句子缺主语
fn spoken_name(username: &str) -> String {
    let name = strip_noise(username);
    let name = name.trim();
    if has_readable_content(name) {
        name.to_string()
    } else {
        "观众".into()
    }
}

/// 朗读前清洗（prd.md §24）：丢掉噪声字符与链接、折叠连续重复字符、规整空白
pub(super) fn clean_for_speech(text: &str) -> String {
    collapse_repeats(&strip_noise(text))
        .split_whitespace()
        .filter(|token| !is_link(token))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 只剔除噪声字符（昵称这类短文本用：不该折叠重复字符，也不该丢链接）
fn strip_noise(text: &str) -> String {
    text.chars().filter(|ch| !is_noise(*ch)).collect()
}

/// 同一字符连续超过 MAX_REPEAT 次的部分丢掉（「哈哈哈哈哈哈哈」只读三个）
fn collapse_repeats(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev = '\0';
    let mut repeat = 0u32;
    for ch in text.chars() {
        if ch == prev {
            repeat += 1;
            if repeat > MAX_REPEAT {
                continue;
            }
        } else {
            prev = ch;
            repeat = 1;
        }
        out.push(ch);
    }
    out
}

/// 会被引擎念出「名字」而不是当作内容的字符。
///
/// 本机对 zh-CN-XiaoxiaoNeural 逐字实测得出（`scripts/edge-tts-probe.ps1`，Δ 字节数见 `Task/findings.md`）：
/// - `_`/`＿`→下划线、`\`/`＼`→反斜杠、`#`/`＃`→井号、`*`/`＊`→星号、`=`/`＝`→等号
/// - `%`/`％`→百分号、`$`/`＄`→美元、`@`/`＠`→at、`°`→度
/// - `ω`→欧米伽、`ε`→epsilon、`Σ`→sigma、`∀`→任意、`ψ`/`π`→字母名（颜文字常客）
///
/// 实测全角与半角行为完全一致，故两套一并列出。实测发音**有意义**、故保留的：
/// `×`→乘以、`÷`→除以、`±`→正负、`○`→零；实测**不出声**、无需处理的：
/// `~ ～ ! ? - . , / | ^ < > [ ] ( ) { } : ; · ・ … ※ ★ ☆ □ △ ▲ ● ◇ ◆ ▁ ﹏ ╯ ╰ ﾟ ｀`
const NAMED_CHARS: &str = "_\\#*=%@$°＿＼＃＊＝％＄＠ωεΣ∀ψπ";

/// 朗读前丢弃的字符：控制字符、emoji / 图形符号、以及会被念出名字的字符
fn is_noise(ch: char) -> bool {
    if NAMED_CHARS.contains(ch) {
        return true;
    }
    let code = ch as u32;
    code < 0x20
        || code == 0x7F
        || code == 0x200D
        || code == 0xFFFD
        || (0xFE00..=0xFE0F).contains(&code)
        || (0x2190..=0x21FF).contains(&code)
        || (0x2600..=0x27BF).contains(&code)
        || (0x2B00..=0x2BFF).contains(&code)
        || (0x1F000..=0x1FAFF).contains(&code)
}

/// 网址不值得念，整段丢掉
fn is_link(token: &str) -> bool {
    let lower = token.to_lowercase();
    lower.starts_with("http")
        || lower.starts_with("www.")
        || lower.contains("://")
        || lower.contains(".com/")
        || lower.contains(".cn/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn danmaku(content: &str) -> Danmaku {
        Danmaku {
            id: "1".into(),
            username: "小明".into(),
            content: content.into(),
            timestamp: 0,
            level: None,
            fansclub_level: None,
        }
    }

    #[test]
    fn collapses_repeated_characters() {
        assert_eq!(clean_for_speech("哈哈哈哈哈哈哈哈"), "哈哈哈");
        assert_eq!(clean_for_speech("6"), "6");
        assert_eq!(clean_for_speech("6666666到飞起"), "666到飞起");
    }

    #[test]
    fn strips_emoji_and_links_and_whitespace() {
        assert_eq!(clean_for_speech("主播好棒😄🎉"), "主播好棒");
        assert_eq!(clean_for_speech("看这里 https://b23.tv/abc 谢谢"), "看这里 谢谢");
        assert_eq!(clean_for_speech("  多个   空格  "), "多个 空格");
    }

    #[test]
    fn strips_symbols_the_engine_would_name_out_loud() {
        // `_` 会被念成「下划线」：只忽略这个字符，整条弹幕照读
        assert_eq!(clean_for_speech("主播_好棒"), "主播好棒");
        assert_eq!(clean_for_speech("_顶_你_"), "顶你");
        assert_eq!(clean_for_speech("C_语言 ＃1 星＊号"), "C语言 1 星号");
        // 只由这类符号组成 → 确实没东西可念，交由上层丢弃
        assert!(clean_for_speech("_＿_＼").is_empty());
        // 实测不出声的符号保留（波浪号是语气延伸，不该删）
        assert_eq!(clean_for_speech("你好~!!"), "你好~!!");
        // 颜文字里的希腊字母会被念成字母名（「欧米伽」），度号会被念成「度」
        assert_eq!(clean_for_speech("(╯°□°)╯ε"), "(╯□)╯");
    }

    #[test]
    fn symbols_do_not_break_repeat_collapsing() {
        assert_eq!(clean_for_speech("哈哈哈_哈哈哈"), "哈哈哈");
    }

    #[test]
    fn build_text_follows_output_switches() {
        let plain = TtsConfig::default();
        let d = danmaku("谢谢礼物");
        assert_eq!(build_text(&d, &plain), "谢谢礼物");

        let with_user = TtsConfig {
            read_username: true,
            ..TtsConfig::default()
        };
        assert_eq!(build_text(&d, &with_user), "小明说 谢谢礼物");
    }

    #[test]
    fn build_text_truncates_content_only() {
        let config = TtsConfig {
            read_username: true,
            max_len: 5,
            ..TtsConfig::default()
        };
        let mut d = danmaku("今天天气真不错呢");
        d.username = "森罗万象之主宰".into();
        assert_eq!(build_text(&d, &config), "森罗万象之主宰说 今天天气真");
    }

    #[test]
    fn cleans_underscores_in_username_too() {
        // 昵称 `雨月云__` 里的下划线同样会被念成「下划线」，昵称也走噪声剔除
        let config = TtsConfig {
            read_username: true,
            ..TtsConfig::default()
        };
        let mut d = danmaku("好好好");
        d.username = "雨月云__".into();
        assert_eq!(build_text(&d, &config), "雨月云说 好好好");
    }

    #[test]
    fn skips_username_that_is_all_noise() {
        let config = TtsConfig {
            read_username: true,
            ..TtsConfig::default()
        };
        let mut d = danmaku("好好好");
        d.username = "_＿_".into();
        assert_eq!(build_text(&d, &config), "好好好");
    }

    #[test]
    fn user_name_keeps_repeats() {
        // 昵称里的重复字符不折叠（那是用户的名字，不是拖长音）
        assert_eq!(strip_noise("哈哈哈哈哈__"), "哈哈哈哈哈");
    }

    #[test]
    fn empty_content_after_cleaning_is_not_spoken_at_all() {
        // 正文没东西可念时，不该只剩下「小明说」这种空壳
        let config = TtsConfig {
            read_username: true,
            ..TtsConfig::default()
        };
        assert_eq!(build_text(&danmaku("😄🎉"), &config), "");
        assert_eq!(build_text(&danmaku("__"), &config), "");
        assert_eq!(build_text(&danmaku("https://b23.tv/abc"), &config), "");
        assert_eq!(build_text(&danmaku("   "), &config), "");
    }

    #[test]
    fn punctuation_only_content_is_not_spoken_at_all() {
        // 「？」「！」这类符号引擎不出声，只剩用户名就会念成「小明说」+ 空白
        let config = TtsConfig {
            read_username: true,
            ..TtsConfig::default()
        };
        assert_eq!(build_text(&danmaku("?"), &config), "");
        assert_eq!(build_text(&danmaku("？？？"), &config), "");
        assert_eq!(build_text(&danmaku("！ 。 ~"), &config), "");
        // 标点里夹了字就得念，只把无内容的标点留在后面无妨
        assert_eq!(build_text(&danmaku("?好"), &config), "小明说 ?好");
    }

    #[test]
    fn punctuation_only_username_is_skipped() {
        let config = TtsConfig {
            read_username: true,
            ..TtsConfig::default()
        };
        let mut d = danmaku("谢谢你");
        d.username = "？？".into();
        assert_eq!(build_text(&d, &config), "谢谢你");
    }

    #[test]
    fn sensitive_word_drops_whole_danmaku() {
        let config = TtsConfig {
            read_username: true,
            filter: DanmakuFilter {
                enable_sensitive: true,
                sensitive_words: vec!["哈哈".into()],
                ..DanmakuFilter::default()
            },
            ..TtsConfig::default()
        };
        let d = danmaku("哈哈哈哈");
        // 筛选在组装文案之前：命中敏感词就整条丢掉，不会念出「小明说」
        assert!(!matches_filter(&d, &config.filter));
    }

    fn backing(username: &str, gift_name: &str, num: u32) -> Backing {
        Backing {
            id: "1".into(),
            kind: crate::douyin::event::BackingKind::Gift,
            uid: 1,
            username: username.into(),
            gift_name: gift_name.into(),
            gift_id: 100,
            num,
            amount_fen: 100,
            timestamp: 0,
            level: None,
        }
    }

    #[test]
    fn gift_text_reads_username_and_count() {
        let five = backing("老板A", "辣条", 5);
        assert_eq!(build_backing_text(&five), "感谢老板A送的5个辣条");
        let one = backing("老板A", "辣条", 1);
        assert_eq!(build_backing_text(&one), "感谢老板A送的1个辣条");
    }

    #[test]
    fn unreadable_name_or_gift_falls_back() {
        // 纯符号昵称 → 回退「观众」
        let noise_name = backing("_＿_", "辣条", 3);
        assert_eq!(build_backing_text(&noise_name), "感谢观众送的3个辣条");
        // 礼物名清洗后为空 → 不出现「送的3个」空尾巴
        let noise_gift = backing("老板A", "***", 3);
        assert_eq!(build_backing_text(&noise_gift), "感谢老板A送的3个礼物");
    }

    #[test]
    fn filter_matches_chosen_level() {
        let high_level = DanmakuFilter {
            enable_level: true,
            level_min: 10,
            ..DanmakuFilter::default()
        };
        let mut d = danmaku("你好");
        // 等级拿不到（None）→ 当成不够级，不过滤时不该放过
        assert!(!matches_filter(&d, &high_level));
        d.level = Some(9);
        assert!(!matches_filter(&d, &high_level));
        d.level = Some(10);
        assert!(matches_filter(&d, &high_level));
        // 敏感词屏蔽独立叠加，高等级也不豁免
        let blocked = DanmakuFilter {
            enable_level: true,
            level_min: 10,
            enable_sensitive: true,
            sensitive_words: vec!["广告".into()],
            ..DanmakuFilter::default()
        };
        assert!(!matches_filter(&danmaku("加群 广告"), &blocked));
    }
}
