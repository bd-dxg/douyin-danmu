//! 抖音房间解析：短号（`web_rid`）→ 真实 `room_id` + `ttwid`
//!
//! 实测两步（见 `probe/README.md`）：
//! 1. `GET https://live.douyin.com/{短号}` —— 从 Set-Cookie 抠 `ttwid`（WS 握手必需）
//! 2. `GET https://live.douyin.com/webcast/room/web/enter/?web_rid={短号}` —— 返回里的
//!    `id_str` 才是真实 room_id（19 位）；短号**不能**直接连 WS
//!
//! 这两步都**不需要**签名（`a_bogus` 只出现在推荐流等接口上）。

use reqwest::Client;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// 解析结果
#[derive(Debug, Clone)]
pub struct DouyinRoomInfo {
    /// 真实 room_id（19 位长号），WS 连接用
    pub room_id: String,
    /// WS 握手与后续请求都要带的 cookie（`ttwid` 必留，登录态可选）
    pub cookie: String,
    /// 主播昵称（= 响应里的 `data.user.nickname`，已实测：765790908625 → 「若水🎙️（经典老歌）」）
    /// 拿不到为 None，前端回退显示房间号
    pub uname: Option<String>,
    /// 是否正在直播（`data.data[0].status`，实测 2=直播中 / 4=已结束）
    pub live: bool,
}

/// 解析失败的原因（都直接给用户看，所以文案要能指导下一步）
#[derive(Debug)]
pub enum ResolveError {
    /// 网络/HTTP 失败
    Network(String),
    /// 拿不到 ttwid（抖音风控或接口变了）
    NoTtwid,
    /// 拿不到 room_id（房间不存在或已下播）
    NoRoomId,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Network(e) => write!(f, "请求抖音失败：{e}"),
            ResolveError::NoTtwid => write!(f, "没有拿到 ttwid（可能被风控），稍后重试"),
            ResolveError::NoRoomId => write!(f, "没有解析出房间号，确认房间号正确且正在直播"),
        }
    }
}

/// 解析短号。
///
/// `login_cookie` 是登录态 cookie（可为空）——**务必保留新鲜的 ttwid**：
/// 实测把带登录态的整串 cookie 直接替换掉 ttwid 会让 WS 握手返回
/// `200 + handshake-msg: http: named cookie not present`。
pub async fn resolve(client: &Client, web_rid: &str, login_cookie: Option<&str>) -> Result<DouyinRoomInfo, ResolveError> {
    let home = format!("https://live.douyin.com/{web_rid}");
    let resp = client
        .get(&home)
        .header("user-agent", UA)
        .header("referer", "https://live.douyin.com/")
        .header("accept-language", "zh-CN,zh;q=0.9")
        .send()
        .await
        .map_err(|e| ResolveError::Network(e.to_string()))?;

    let ttwid = ttwid_from_cookies(resp.headers())?;
    let _ = resp.text().await; // 主页 HTML 不用，但要读完才能复用连接

    let cookie = merge_cookie(login_cookie, &ttwid);

    let enter = format!(
        "https://live.douyin.com/webcast/room/web/enter/?aid=6383&app_name=douyin_web&live_id=1\
         &device_platform=web&language=zh-CN&cookie_enabled=true&screen_width=1920&screen_height=1080\
         &browser_language=zh-CN&browser_platform=Win32&browser_name=Chrome&browser_version=126.0.0.0\
         &web_rid={web_rid}&enter_from=web_live&is_need_double_stream=false"
    );
    let body = client
        .get(&enter)
        .header("user-agent", UA)
        .header("referer", &home)
        .header("accept-language", "zh-CN,zh;q=0.9")
        .header("cookie", &cookie)
        .send()
        .await
        .map_err(|e| ResolveError::Network(e.to_string()))?
        .text()
        .await
        .map_err(|e| ResolveError::Network(e.to_string()))?;

    let room_id = parse_room_id(&body).ok_or(ResolveError::NoRoomId)?;
    let uname = parse_uname(&body);
    let live = parse_live(&body);

    Ok(DouyinRoomInfo {
        room_id,
        cookie,
        uname,
        live,
    })
}

/// 从 enter 响应里取主播昵称（`data.user.nickname`）
///
/// 实测：`data.user` 就是房主（`id_str` 与 `room.owner_user_id_str` 同源），
/// 它的 `nickname` 就是主播名；房间对象（`data.data[0]`）里反而没有昵称。
fn parse_uname(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.pointer("/data/user/nickname")
        .and_then(|n| n.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// 是否直播中（`data.data[0].status == 2`；实测 4 = 已结束）
fn parse_live(json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.pointer("/data/data/0/status").and_then(|s| s.as_u64()))
        == Some(2)
}

/// 取真实 room_id：先走精确路径 `data.data[0].id_str`，取不到再回退手扫
fn parse_room_id(json: &str) -> Option<String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
        if let Some(id) = v.pointer("/data/data/0/id_str").and_then(|s| s.as_str()) {
            if (15..=25).contains(&id.len()) {
                return Some(id.to_owned());
            }
        }
    }
    find_id_str(json)
}

/// 从响应头里抠 ttwid
fn ttwid_from_cookies(headers: &reqwest::header::HeaderMap) -> Result<String, ResolveError> {
    for value in headers.get_all(reqwest::header::SET_COOKIE).iter() {
        let Ok(raw) = value.to_str() else { continue };
        if let Some(rest) = raw.strip_prefix("ttwid=") {
            let end = rest.find(';').unwrap_or(rest.len());
            let v = rest[..end].trim();
            if !v.is_empty() {
                return Ok(v.to_owned());
            }
        }
    }
    Err(ResolveError::NoTtwid)
}

/// 合并 cookie：登录态在前，ttwid 只在登录态里没有时才追加（不覆盖）
pub fn merge_cookie(login_cookie: Option<&str>, ttwid: &str) -> String {
    match login_cookie {
        None => format!("ttwid={ttwid}"),
        Some(c) if c.trim().is_empty() => format!("ttwid={ttwid}"),
        Some(c) if c.contains("ttwid=") => c.to_owned(),
        Some(c) => format!("{}; ttwid={ttwid}", c.trim_end_matches(';').trim_end()),
    }
}

/// 从 enter 接口的 JSON 里取 `"id_str":"<15~25 位数字>"`。
///
/// 不用正则：整段响应 60 万字符，手扫一遍足够，也省一个依赖。
/// 只想认 15~25 位数字，避免把响应里其他实体的 `id_str`（如用户 id）误当房间号。
pub fn find_id_str(json: &str) -> Option<String> {
    let mut from = 0;
    while let Some(rel) = json[from..].find("\"id_str\"") {
        let at = from + rel + "\"id_str\"".len();
        from = at;
        let rest = &json[at..];
        // 跳过空白与冒号
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix(':') else { continue };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('"') else { continue };
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if (15..=25).contains(&digits) {
            return Some(rest[..digits].to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 能从_enter_响应里捞出房间号() {
        let json = r#"{"data":{"data":[{"id_str":"7686161414943623974","title":"x"}],"user":{"id_str":"94151964326"}}}"#;
        // 第一个命中就是房间的（15~25 位），用户 id 是 11 位会被跳过
        assert_eq!(find_id_str(json).unwrap(), "7686161414943623974");
    }

    #[test]
    fn 从_enter_响应里抠出主播名与直播状态() {
        // 字段路径实测来自真机响应（只留了用到的部分）
        let json = r#"{"status_code":0,"data":{"data":[{"id_str":"7686161414943623974","status":4,"title":"只唱歌"}],"user":{"nickname":"若水🎙️（经典老歌）","id_str":"97649156416"}}}"#;
        assert_eq!(parse_room_id(json).as_deref(), Some("7686161414943623974"));
        assert_eq!(parse_uname(json).as_deref(), Some("若水🎙️（经典老歌）"));
        assert!(!parse_live(json), "status=4 是已结束");

        let live_json = r#"{"data":{"data":[{"id_str":"7686161414943623974","status":2}],"user":{"nickname":"某主播"}}}"#;
        assert!(parse_live(live_json), "status=2 是直播中");
    }

    #[test]
    fn 响应里没有主播名时不报错() {
        let json = r#"{"data":{"data":[{"id_str":"7686161414943623974"}]}}"#;
        assert_eq!(parse_uname(json), None);
        assert!(!parse_live(json));
        assert_eq!(parse_room_id(json).as_deref(), Some("7686161414943623974"));
    }

    #[test]
    fn 短_id_str_被跳过() {
        let json = r#"{"user":{"id_str":"94151964326"},"room":{"id_str":"7686161414943623974"}}"#;
        assert_eq!(find_id_str(json).unwrap(), "7686161414943623974");
    }

    #[test]
    fn 没有_id_str_时返回_none() {
        assert!(find_id_str(r#"{"data":{}}"#).is_none());
    }

    #[test]
    fn cookie_合并保留_ttwid() {
        // 登录态里没有 ttwid → 追加
        assert_eq!(merge_cookie(Some("sessionid=abc"), "T1"), "sessionid=abc; ttwid=T1");
        // 登录态里已有 ttwid → 不覆盖（用用户自己的）
        assert_eq!(merge_cookie(Some("sessionid=abc; ttwid=U1"), "T1"), "sessionid=abc; ttwid=U1");
        // 无登录态 → 只有 ttwid
        assert_eq!(merge_cookie(None, "T1"), "ttwid=T1");
        assert_eq!(merge_cookie(Some("  "), "T1"), "ttwid=T1");
    }
}
