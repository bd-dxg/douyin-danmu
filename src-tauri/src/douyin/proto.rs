//! 抖音直播弹幕的 protobuf 编解码（手写 varint，不引第三方 protobuf 库）
//!
//! 字段号全部实测过（探针 `probe/`，权威表在 `probe/reference/douyin.proto`）：
//! - `PushFrame`：f1=seq_id, f2=log_id, f5=headers, f7=payload_type, f8=payload(gzip)
//! - `Response`： f1=messages(repeated), f5=internal_ext, f9=need_ack
//! - `Message`：  f1=method, f2=payload
//!
//! **未知字段一律跳过**：抖音随时加字段，不能因为不认识就把整帧丢掉。

use std::io::Read;

const WIRE_VARINT: u8 = 0;
const WIRE_64BIT: u8 = 1;
/// len-delimited（嵌套消息 / 字符串 / 字节流）—— `parser` 递归找嵌套字段时也用它
pub const WIRE_LEN: u8 = 2;
const WIRE_32BIT: u8 = 5;

/// 单条心跳/ack 用的 gzip 空体（常量，避免每次重算）
pub const EMPTY_PAYLOAD: &[u8] = &[];

#[derive(Debug, PartialEq, Eq)]
pub enum ProtoError {
    /// 字节流在字段中途断掉
    Truncated,
    /// 不认识的 wire type（protobuf 规范共 6 种，其余属于脏数据）
    BadWireType(u8),
    /// payload 解压失败
    Inflate(String),
}

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoError::Truncated => write!(f, "protobuf 字节流被截断"),
            ProtoError::BadWireType(w) => write!(f, "未知 wire type: {w}"),
            ProtoError::Inflate(e) => write!(f, "payload 解压失败: {e}"),
        }
    }
}

/// 解出的单个字段。varint 字段只看 `varint`，len-delimited 只看 `bytes`。
#[derive(Debug)]
pub struct Field<'a> {
    pub num: u32,
    pub wire: u8,
    pub varint: u64,
    pub bytes: &'a [u8],
}

impl<'a> Field<'a> {
    /// 用 utf8 读字符串字段（非法 utf8 返回 None，不猜）
    pub fn as_str(&self) -> Option<&'a str> {
        std::str::from_utf8(self.bytes).ok()
    }
}

/// 读一个 varint，返回 (值, 剩余字节)
fn read_varint(buf: &[u8]) -> Result<(u64, &[u8]), ProtoError> {
    let mut value: u64 = 0;
    let mut shift: u32 = 0;
    for (i, &b) in buf.iter().enumerate() {
        // u64 最多 10 字节；超过说明字节流是脏的
        if i >= 10 {
            return Err(ProtoError::Truncated);
        }
        value |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok((value, &buf[i + 1..]));
        }
        shift += 7;
    }
    Err(ProtoError::Truncated)
}

/// 把一段字节流解成字段列表
pub fn read_fields(mut buf: &[u8]) -> Result<Vec<Field<'_>>, ProtoError> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        let (key, rest) = read_varint(buf)?;
        buf = rest;
        let num = (key >> 3) as u32;
        let wire = (key & 0x07) as u8;
        match wire {
            WIRE_VARINT => {
                let (v, rest) = read_varint(buf)?;
                buf = rest;
                out.push(Field { num, wire, varint: v, bytes: EMPTY_PAYLOAD });
            }
            WIRE_LEN => {
                let (len, rest) = read_varint(buf)?;
                let len = len as usize;
                if rest.len() < len {
                    return Err(ProtoError::Truncated);
                }
                let (body, rest) = rest.split_at(len);
                buf = rest;
                out.push(Field { num, wire, varint: 0, bytes: body });
            }
            WIRE_64BIT => {
                if buf.len() < 8 {
                    return Err(ProtoError::Truncated);
                }
                buf = &buf[8..];
            }
            WIRE_32BIT => {
                if buf.len() < 4 {
                    return Err(ProtoError::Truncated);
                }
                buf = &buf[4..];
            }
            other => return Err(ProtoError::BadWireType(other)),
        }
    }
    Ok(out)
}

/// 取指定字段号上所有 len-delimited 字段（repeated 用）
pub fn repeated<'a>(fields: &'a [Field<'a>], num: u32) -> Vec<&'a Field<'a>> {
    fields.iter().filter(|f| f.num == num && f.wire == WIRE_LEN).collect()
}

/// 取 varint 字段（字段不存在、或该号上是 len-delimited 时都返回 None）
pub fn varint(fields: &[Field<'_>], num: u32) -> Option<u64> {
    fields
        .iter()
        .rev()
        .find(|f| f.num == num && f.wire == WIRE_VARINT)
        .map(|f| f.varint)
}

/// 取字符串字段
pub fn string(fields: &[Field<'_>], num: u32) -> Option<String> {
    fields
        .iter()
        .rev()
        .find(|f| f.num == num && f.wire == WIRE_LEN)
        .and_then(|f| f.as_str())
        .map(str::to_owned)
}

/// 取嵌套消息字段的原始字节
pub fn message<'a>(fields: &'a [Field<'a>], num: u32) -> Option<&'a [u8]> {
    fields
        .iter()
        .rev()
        .find(|f| f.num == num && f.wire == WIRE_LEN)
        .map(|f| f.bytes)
}

// ---------- 写（只用于 ack 与心跳） ----------

pub fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

pub fn write_varint_field(out: &mut Vec<u8>, num: u32, v: u64) {
    write_varint(out, ((num as u64) << 3) | WIRE_VARINT as u64);
    write_varint(out, v);
}

pub fn write_bytes_field(out: &mut Vec<u8>, num: u32, body: &[u8]) {
    write_varint(out, ((num as u64) << 3) | WIRE_LEN as u64);
    write_varint(out, body.len() as u64);
    out.extend_from_slice(body);
}

pub fn write_str_field(out: &mut Vec<u8>, num: u32, s: &str) {
    write_bytes_field(out, num, s.as_bytes());
}

// ---------- 帧级结构 ----------

/// `PushFrame`：外层帧。`f8`(payload) 是 gzip 过的 `Response`
#[derive(Debug)]
pub struct PushFrame {
    pub log_id: u64,
    pub payload: Vec<u8>,
}

/// 解外层帧。
///
/// 实测两点：`f8` 并不总是带 gzip 头（个别帧是裸明文），所以按魔数判断后决定是否解压；
/// 空 payload（心跳回包）直接得到空 Response。
pub fn decode_push_frame(buf: &[u8]) -> Result<PushFrame, ProtoError> {
    let fields = read_fields(buf)?;
    let log_id = varint(&fields, 2).unwrap_or(0);
    let payload = match message(&fields, 8) {
        None => Vec::new(),
        Some(raw) => inflate(raw)?,
    };
    Ok(PushFrame { log_id, payload })
}

/// gzip 解压；没有 gzip 魔数就原样返回
pub fn inflate(raw: &[u8]) -> Result<Vec<u8>, ProtoError> {
    if raw.len() < 2 || raw[0] != 0x1f || raw[1] != 0x8b {
        return Ok(raw.to_vec());
    }
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(raw)
        .read_to_end(&mut out)
        .map_err(|e| ProtoError::Inflate(e.to_string()))?;
    Ok(out)
}

pub fn gzip(raw: &[u8]) -> Result<Vec<u8>, ProtoError> {
    use flate2::write::GzEncoder;
    use std::io::Write;
    let mut enc = GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(raw).map_err(|e| ProtoError::Inflate(e.to_string()))?;
    enc.finish().map_err(|e| ProtoError::Inflate(e.to_string()))
}

/// `Response`：一批消息
#[derive(Debug, Default)]
pub struct Response {
    /// (method, payload)
    pub messages: Vec<(String, Vec<u8>)>,
    pub internal_ext: String,
    pub need_ack: bool,
}

pub fn decode_response(buf: &[u8]) -> Result<Response, ProtoError> {
    let fields = read_fields(buf)?;
    let mut messages = Vec::new();
    for m in repeated(&fields, 1) {
        let inner = read_fields(m.bytes)?;
        let method = string(&inner, 1).unwrap_or_default();
        let payload = message(&inner, 2).unwrap_or(EMPTY_PAYLOAD).to_vec();
        if !method.is_empty() {
            messages.push((method, payload));
        }
    }
    Ok(Response {
        messages,
        internal_ext: string(&fields, 5).unwrap_or_default(),
        need_ack: varint(&fields, 9).unwrap_or(0) != 0,
    })
}

/// ack 帧：`PushFrame{f2=log_id, f7="ack", f8=internal_ext}`。
///
/// 实测：不回 ack 服务端每 1–2 秒重推同一帧（cursor 不推进）；回了才继续推进。
pub fn build_ack(log_id: u64, internal_ext: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(internal_ext.len() + 24);
    write_varint_field(&mut out, 2, log_id);
    write_str_field(&mut out, 7, "ack");
    write_str_field(&mut out, 8, internal_ext);
    out
}

/// 心跳帧：`PushFrame{f8=gzip(空)}`（实测 40 秒不被踢；saermart 用 `payload_type="hb"` 的
/// PING 帧也能保活，两种都没问题，这里用帧内 payload）
pub fn build_heartbeat() -> Vec<u8> {
    let body = gzip(EMPTY_PAYLOAD).unwrap_or_default();
    let mut out = Vec::with_capacity(body.len() + 4);
    write_bytes_field(&mut out, 8, &body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个 varint 字段
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

    #[test]
    fn 解外层帧能拿到_log_id_与_gzip_payload() {
        let inner = [s(1, "WebcastChatMessage"), s(2, "x")].concat();
        let body = gzip(&inner).unwrap();
        let mut frame = v(2, 424242);
        write_bytes_field(&mut frame, 8, &body);

        let pf = decode_push_frame(&frame).unwrap();
        assert_eq!(pf.log_id, 424242);
        assert_eq!(pf.payload, inner);
    }

    #[test]
    fn 裸明文_payload_不解压() {
        let mut frame = v(2, 1);
        write_bytes_field(&mut frame, 8, b"plain");
        assert_eq!(decode_push_frame(&frame).unwrap().payload, b"plain");
    }

    #[test]
    fn 解响应能拿到方法与_body() {
        let mut msg = s(1, "WebcastChatMessage");
        write_bytes_field(&mut msg, 2, b"BODY");
        let mut resp = Vec::new();
        write_bytes_field(&mut resp, 1, &msg);
        write_str_field(&mut resp, 5, "internal_src:pushserver|seq:1");
        write_varint_field(&mut resp, 9, 1);

        let r = decode_response(&resp).unwrap();
        assert_eq!(r.messages.len(), 1);
        assert_eq!(r.messages[0].0, "WebcastChatMessage");
        assert_eq!(r.messages[0].1, b"BODY");
        assert!(r.need_ack);
        assert!(r.internal_ext.starts_with("internal_src"));
    }

    #[test]
    fn 未知字段被跳过而不是报错() {
        // f99 varint + f98 len + 合法 f1/f2：模拟抖音加字段
        let mut msg = v(99, 7);
        msg.extend(s(98, "unknown"));
        msg.extend(s(1, "WebcastChatMessage"));
        write_bytes_field(&mut msg, 2, b"BODY");
        let mut resp = Vec::new();
        write_bytes_field(&mut resp, 1, &msg);

        let r = decode_response(&resp).unwrap();
        assert_eq!(r.messages[0].0, "WebcastChatMessage");
    }

    #[test]
    fn 截断的字节流返回错误而不是越界() {
        assert_eq!(read_fields(&[0x12, 0x05, 0x01]).unwrap_err(), ProtoError::Truncated);
        assert_eq!(read_fields(&[0x10]).unwrap_err(), ProtoError::Truncated);
    }

    #[test]
    fn ack_帧字段号与实测一致() {
        let ack = build_ack(12345, "ext");
        let fields = read_fields(&ack).unwrap();
        assert_eq!(varint(&fields, 2), Some(12345));
        assert_eq!(string(&fields, 7).as_deref(), Some("ack"));
        assert_eq!(string(&fields, 8).as_deref(), Some("ext"));
    }

    #[test]
    fn 心跳帧能被自己解回空_payload() {
        let hb = build_heartbeat();
        let pf = decode_push_frame(&hb).unwrap();
        assert!(pf.payload.is_empty());
    }
}
