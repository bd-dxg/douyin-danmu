//! 抖音 X-Bogus 签名
//!
//! 实测（见 `probe/README.md`）：签名输入 = **13 个参数按固定顺序**拼 `k=v`、用逗号连接
//! → `MD5` 小写 hex（这个值即 `X-MS-STUB`）→ 交给 webmssdk 的 `getSign` → `X-Bogus`。
//! 同一 md5 每次算出的 X-Bogus 不同（webmssdk 掺了环境指纹），服务端都认。
//!
//! 拼串与 md5 是本模块的活；[`Signer`] 负责「拿 md5 换 X-Bogus」——那一步**必须跑
//! webmssdk 的 JS**（阶段 5 走隐藏 WebView2）。抽象成 trait 是为了让 `ws.rs` 只持有
//! `&dyn Signer`，不认识 WebView2，单测里也能直接注入桩。

use std::future::Future;
use std::pin::Pin;

/// 参与签名的 13 个参数，**顺序即拼接顺序**——顺序变了签名就错，不要重排。
pub const SIGN_PARAM_ORDER: [&str; 13] = [
    "live_id",
    "aid",
    "version_code",
    "webcast_sdk_version",
    "room_id",
    "sub_room_id",
    "sub_channel_id",
    "did_rule",
    "user_unique_id",
    "device_platform",
    "device_type",
    "ac",
    "identity",
];

/// 签名失败的原因（直接进日志，也作为 [`crate::douyin::ws::WsError::Sign`] 的载荷）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignError(pub String);

impl std::fmt::Display for SignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SignError {}

/// 签名器返回的未来（trait 里写不了 `async fn`，装箱省掉 async-trait 依赖）
pub type SignFuture = Pin<Box<dyn Future<Output = Result<String, SignError>> + Send + 'static>>;

/// X-Bogus 签名器：`md5`（= X-MS-STUB）→ `X-Bogus`
pub trait Signer: Send + Sync {
    fn sign(&self, md5: String) -> SignFuture;
}

/// 任何 `Fn(md5) -> Future<Result<X-Bogus, SignError>>` 都能当签名器：
/// 运行时注入走隐藏 WebView2 的实现（阶段 5），单测注入固定串
impl<F, Fut> Signer for F
where
    F: Fn(String) -> Fut + Send + Sync,
    Fut: Future<Output = Result<String, SignError>> + Send + 'static,
{
    fn sign(&self, md5: String) -> SignFuture {
        Box::pin(self(md5))
    }
}

/// 从 WS URL 的 query 里按 [`SIGN_PARAM_ORDER`] 拼出签名输入串。
///
/// 三个前提（任何一条不成立，服务端验签都会失败）：
/// 1. URL 是**加 signature 之前**的那份——`signature` 不在 13 个参数里，所以拼串不受影响
/// 2. 缺失的参数按空串参与（`sub_room_id` / `device_type` / `ac` 实测就不在 URL 上）
/// 3. 这 13 个参数的值**不能带百分号转义**：服务端按解码后的值验签，而我们签的是原文。
///    当前 URL 施工（`ws::build_ws_url`）里这 13 个值全是纯 ASCII，由单测守这条线。
pub fn sign_input(url: &str) -> String {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    SIGN_PARAM_ORDER
        .iter()
        .map(|key| {
            let value = query
                .split('&')
                .find_map(|kv| {
                    let (name, value) = kv.split_once('=')?;
                    (name == *key).then_some(value)
                })
                .unwrap_or("");
            format!("{key}={value}")
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// 签名输入 → `X-MS-STUB`（= MD5 小写 hex）
pub fn x_ms_stub(input: &str) -> String {
    use md5::Digest;
    md5::Md5::digest(input.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `encodeURIComponent` 等价实现：保留 `A-Za-z0-9 - _ . ! ~ * ' ( )`
///
/// 两个用处：URL 里的 `internal_ext`（含 `|` 与 `:`，不转义连 URI 都解析不出来）
/// 与 X-Bogus 本身（实测样本含 `+` 与 `/`）。
pub fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')')
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::douyin::ws::build_ws_url;

    #[test]
    fn 签名输入是_13_个参数按固定顺序拼接() {
        let url = build_ws_url("7686161414943623974", "1700000000000000000");
        assert_eq!(
            sign_input(&url),
            "live_id=1,aid=6383,version_code=180800,webcast_sdk_version=1.3.0,\
             room_id=7686161414943623974,sub_room_id=,sub_channel_id=,did_rule=3,\
             user_unique_id=1700000000000000000,device_platform=web,device_type=,ac=,identity=audience"
        );
    }

    #[test]
    fn 签名输入里不允许出现百分号转义() {
        // 转义值会让服务端按解码后验签、而我们签的是原文 —— 一旦有人给
        // build_ws_url 的这 13 个参数加了转义，这条断言会先炸
        let url = build_ws_url("7686161414943623974", "1700000000000000000");
        assert!(!sign_input(&url).contains('%'));
    }

    #[test]
    fn 没有_query_时参数全为空串() {
        assert_eq!(sign_input("wss://x/y"), "live_id=,aid=,version_code=,webcast_sdk_version=,\
             room_id=,sub_room_id=,sub_channel_id=,did_rule=,user_unique_id=,device_platform=,\
             device_type=,ac=,identity=");
    }

    #[test]
    fn 签名输入端到端与探针一致() {
        // 期望值来源：`probe/node/ws-probe.cjs` 的 buildUrl + paramString + node crypto md5
        //（2026-09 本机实跑，同一 room_id / user_unique_id）。改了 URL 参数或
        // SIGN_PARAM_ORDER，这里会先炸 —— 而不是到真机握手时才报 auth failed
        let url = build_ws_url("7686161414943623974", "1700000000000000000");
        assert_eq!(x_ms_stub(&sign_input(&url)), "5af9eb2bbced94bfd670f58fd47aced2");
    }

    #[test]
    fn md5_与已知向量一致() {
        assert_eq!(x_ms_stub(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(x_ms_stub("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn encode_component_语义() {
        // 未保留字符原样保留
        assert_eq!(encode_component("aZ0-_.!~*'()"), "aZ0-_.!~*'()");
        // internal_ext 里的 | 与 :，以及 X-Bogus 里的 + 与 /
        assert_eq!(encode_component("a|b:c"), "a%7Cb%3Ac");
        assert_eq!(encode_component("6d+F41n7DQiE4P5u/"), "6d%2BF41n7DQiE4P5u%2F");
        // 中文按 UTF-8 逐字节转义
        assert_eq!(encode_component("中"), "%E4%B8%AD");
    }

    #[test]
    fn 闭包可以作为签名器注入() {
        // 与仓库其余联网测试同一写法：用 tauri 的运行时 block_on，不引 tokio 的 rt feature
        tauri::async_runtime::block_on(async {
            let stub = |md5: String| async move { Ok(format!("bogus-{md5}")) };
            let signer: &dyn Signer = &stub;
            let got = signer.sign("abc".to_owned()).await.unwrap();
            assert_eq!(got, "bogus-abc");
        });
    }
}
