//! 「已关注且正在开播」列表的数据源（主界面手动刷新）
//!
//! **一个接口就够**：`POST https://www.douyin.com/webcast/feed/follow_top/`
//! —— www 关注页「正在直播(N)」那一块自己调的接口（页面每 120 秒调一次刷新）。
//!
//! 实测结论（2026-10，口径见 `probe/README.md`）：
//! - **不需要签名**：query 只要 `device_platform=webapp&aid=6383&channel=channel_pc_web`，
//!   body 只要 `source_key=web_homepage_follow_top&follow_session_id=<游标>`。
//!   页面真实请求里的 `a_bogus` / `msToken` / `uifid` / `x-secsdk-csrf-token` **全部可以不给**
//!   （少一个就回 `status_code=10011 Request params error` 的那个必填项其实是 body 里的
//!   `source_key`，不是签名）
//! - 返回里每条就是一个直播间：`data.owner.nickname`（主播名）、`web_rid`（**直播间短号**，
//!   不是抖音号）、`data.id_str`（真实 room_id）
//! - 翻页：`extra.follow_session_id` + `extra.has_more`，首次传 0
//!
//! 早先用的是两步（关注列表 `/webcast/user/follow/list/` + 逐个 uid 探活
//! `/webcast/distribution/check_user_live_status/`），已废弃：那个探活接口**一次只认第一个
//! uid**，关注 500 人就得打 500 个请求。

use reqwest::Client;
use std::collections::HashSet;

/// 关注在播列表接口（www.douyin.com；`device_platform=webapp` 是它的必填 query）
const FOLLOW_TOP_URL: &str = "https://www.douyin.com/webcast/feed/follow_top/\
?device_platform=webapp&aid=6383&channel=channel_pc_web";
/// body 里的场景标识（必填：少了它服务端回 `Request params error`）
const SOURCE_KEY: &str = "web_homepage_follow_top";
/// 最多翻页数（一页就是当前在播的所有人，正常一两页结束）
const MAX_PAGES: usize = 10;
/// 未登录 / 登录态失效的业务码
const NOT_LOGIN_CODE: i64 = 20003;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// 一条「已关注且在播」记录
#[derive(Debug, Clone, serde::Serialize)]
pub struct FollowingLive {
    /// 主播 uid
    pub uid: i64,
    /// 主播昵称（`data.owner.nickname`）
    pub uname: String,
    /// 直播间短号（`web_rid`）：点击直连时交给 `connect_room`
    pub web_rid: String,
    /// 真实 room_id（19 位，仅用于界面上标明是哪个房间）
    pub room_id: String,
}

/// 请求这些接口用的客户端（超时短一点：界面上是「查询中…」，卡住要让用户早看到结果）
pub fn http_client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| Client::new())
}

/// 拉一次「已关注且正在开播」的列表（自己在播的主播有变动时按 `follow_session_id` 翻页）
pub async fn list_following_live(client: &Client, cookies: &str) -> Result<Vec<FollowingLive>, String> {
    let mut out: Vec<FollowingLive> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    let mut cursor = 0i64;
    for _ in 0..MAX_PAGES {
        // 页面自己会多带 enter_source / need_map / need_pinned_info，实测可省；这里照抄全套，
        // 免得哪天服务端按这些字段切换返回结构
        let body = format!(
            "source_key={SOURCE_KEY}&follow_session_id={cursor}\
&enter_source=homepage_pc_followtop&need_map=1&need_pinned_info=0"
        );
        let resp = client
            .post(FOLLOW_TOP_URL)
            .header("user-agent", UA)
            .header("referer", "https://www.douyin.com/follow")
            .header("accept", "application/json, text/plain, */*")
            .header("cookie", cookies)
            .header("content-type", "application/x-www-form-urlencoded; charset=UTF-8")
            .body(body)
            .send()
            .await
            .map_err(|e| format!("请求抖音失败：{e}"))?
            .json::<serde_json::Value>()
            .await
            .map_err(|e| format!("抖音响应解析失败：{e}"))?;

        let page = parse_page(&resp)?;
        out.extend(page.items.into_iter().filter(|f| seen.insert(f.uid)));
        // 翻页：has_more 为假就停；游标不前进（服务端给 0 或原值）也要停，否则原地打转
        if !page.has_more || page.cursor == 0 || page.cursor == cursor {
            break;
        }
        cursor = page.cursor;
    }
    Ok(out)
}

/// 「关注在播」的一页
struct FollowTopPage {
    items: Vec<FollowingLive>,
    has_more: bool,
    cursor: i64,
}

/// 解一页响应：`data[]` 是直播间卡片，分页信息在 `extra` 里
fn parse_page(v: &serde_json::Value) -> Result<FollowTopPage, String> {
    let data = ok_data(v)?;
    Ok(FollowTopPage {
        items: data.as_array().into_iter().flatten().filter_map(parse_item).collect(),
        has_more: v.pointer("/extra/has_more").and_then(|h| h.as_bool()) == Some(true),
        cursor: v
            .pointer("/extra/follow_session_id")
            .and_then(|c| c.as_i64())
            .unwrap_or(0),
    })
}

/// 解一张直播间卡片：`data` 是房间对象，短号在卡片与 `owner` 上都有一份
///
/// 不按卡片上的 `live_status` 过滤：实测同一个房间会一闪一闪地给 0/1，这个接口本身返回的
/// 就是「正在直播」列表，按它过滤反而会把在播的主播漏掉。
fn parse_item(item: &serde_json::Value) -> Option<FollowingLive> {
    let room = item.get("data")?;
    let room_id = room.get("id_str").and_then(|s| s.as_str()).filter(|s| !s.is_empty())?;
    let owner = room.get("owner")?;
    let uname = owner.get("nickname").and_then(|n| n.as_str()).unwrap_or("").trim();
    if uname.is_empty() {
        return None;
    }
    let uid = room
        .get("owner_user_id")
        .and_then(|u| u.as_i64())
        .or_else(|| {
            owner
                .get("id_str")
                .and_then(|s| s.as_str())
                .and_then(|s| s.parse::<i64>().ok())
        })?;
    let web_rid = item
        .get("web_rid")
        .and_then(|s| s.as_str())
        .or_else(|| owner.get("web_rid").and_then(|s| s.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some(FollowingLive {
        uid,
        uname: uname.to_owned(),
        web_rid: web_rid.to_owned(),
        room_id: room_id.to_owned(),
    })
}

/// 校验 `status_code` 并取出 `data`（抖音的这类接口失败时也给 200）
fn ok_data(v: &serde_json::Value) -> Result<serde_json::Value, String> {
    let code = v.get("status_code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if code == 0 {
        return Ok(v.get("data").cloned().unwrap_or(serde_json::Value::Null));
    }
    if code == NOT_LOGIN_CODE {
        return Err("登录态已失效，请重新扫码登录".into());
    }
    let msg = v
        .pointer("/data/message")
        .and_then(|m| m.as_str())
        .or_else(|| v.pointer("/data/prompts").and_then(|m| m.as_str()))
        .unwrap_or("");
    Err(format!("抖音接口返回 status_code={code}（{msg}）"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 业务码非零时给出错误() {
        let v = json!({ "status_code": 20003, "data": { "message": "User doesn't login" } });
        assert_eq!(ok_data(&v).unwrap_err(), "登录态已失效，请重新扫码登录");

        let v = json!({ "status_code": 10011, "data": { "message": "Request params error" } });
        assert!(ok_data(&v).unwrap_err().contains("10011"));
        assert!(ok_data(&v).unwrap_err().contains("Request params error"));

        let v = json!({ "status_code": 0, "data": [] });
        assert_eq!(ok_data(&v).unwrap(), json!([]));
    }

    #[test]
    fn 一页解出主播名与直播间短号() {
        // 真机样本的裁剪版：昵称在 data.owner.nickname，短号卡片与 owner 上各一份
        let v = json!({
            "status_code": 0,
            "data": [{
                "type": 2,
                "live_status": 1,
                "web_rid": "639280210755",
                "data": {
                    "id_str": "7697599161746983716",
                    "owner_user_id": 4251138797077433u64,
                    "title": "又是活力满满的一天，",
                    "owner": { "nickname": "贱贱", "web_rid": "639280210755", "short_id": 62876576864u64 }
                }
            }],
            "extra": { "has_more": true, "follow_session_id": 1792242379, "total": 1 }
        });
        let page = parse_page(&v).unwrap();
        assert!(page.has_more);
        assert_eq!(page.cursor, 1792242379);
        assert_eq!(page.items.len(), 1);
        let f = &page.items[0];
        assert_eq!(f.uid, 4251138797077433);
        assert_eq!(f.uname, "贱贱");
        assert_eq!(f.web_rid, "639280210755");
        assert_eq!(f.room_id, "7697599161746983716");
    }

    #[test]
    fn 卡片缺字段时跳过而不报错() {
        let v = json!({
            "status_code": 0,
            // 非直播卡片（没有 data）/ 房间没有 owner / 昵称为空 / 没有短号：都跳过
            "data": [
                { "type": 1 },
                { "data": { "id_str": "1" } },
                { "data": { "id_str": "2", "owner_user_id": 2, "owner": { "nickname": "  " } } },
                { "data": { "id_str": "3", "owner_user_id": 3, "owner": { "nickname": "A" } } }
            ],
            "extra": { "has_more": false, "follow_session_id": 0 }
        });
        let page = parse_page(&v).unwrap();
        assert!(page.items.is_empty());
        assert_eq!(page.cursor, 0);
    }

    #[test]
    fn 没有_owner_user_id_时用_owner_id_str_兜底() {
        let v = json!({
            "status_code": 0,
            "data": [{
                "web_rid": "123",
                "data": { "id_str": "9", "owner": { "nickname": "A", "id_str": "888", "web_rid": "123" } }
            }],
            "extra": { "has_more": false, "follow_session_id": 0 }
        });
        let page = parse_page(&v).unwrap();
        assert_eq!(page.items[0].uid, 888);
    }
}
