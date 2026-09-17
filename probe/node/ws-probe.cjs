// 抖音弹幕协议探针：解析房间 → 签名 → 连 WS → 收帧 → 回 ack。
// 用法：node ws-probe.cjs <直播间短号>（默认 256438100956）
// 目的：验证 X-Bogus 是否被服务端接受（握手 101），以及各 protobuf 字段号是否与 DanmuFree 记录一致。
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const zlib = require('node:zlib');
const WebSocket = require('ws');

const SIGN_DIR = path.join(__dirname, '..', 'sign');
const RID = process.argv[2] || '256438100956';
const DURATION_SEC = Number(process.argv[3] || 30);
// 对照实验：danmufree（原样） vs saermart（另一套连接参数）
// 登录态（可选）：设了就用它，否则纯匿名。只在内存里传递，**不写日志、不落盘**
const COOKIE = process.env.DY_COOKIE || '';
const cookieNames = COOKIE
  ? COOKIE.split(';').map((s) => s.split('=')[0].trim()).filter(Boolean)
  : [];
const VARIANT = process.env.PROBE_VARIANT || 'danmufree';
const UA =
  'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36';

// ---------- 1. 签名（jsdom 版 sign.js，与 DanmuFree 一致） ----------
const signSrc = fs.readFileSync(path.join(SIGN_DIR, 'sign.js'), 'utf8');
function getXbogus(md5) {
  const wrapped =
    '(function(){\n' + signSrc + "\n;return getSign({'X-MS-STUB':" + JSON.stringify(md5) + "});\n})()";
  const r = eval(wrapped);
  return (r && r['X-Bogus']) || '';
}

const PARAM_ORDER = [
  'live_id', 'aid', 'version_code', 'webcast_sdk_version', 'room_id', 'sub_room_id',
  'sub_channel_id', 'did_rule', 'user_unique_id', 'device_platform', 'device_type', 'ac', 'identity',
];

function parseCookie(s) {
  const m = new Map();
  for (const kv of s.split(';')) {
    const i = kv.indexOf('=');
    if (i > 0) m.set(kv.slice(0, i).trim(), kv.slice(i + 1).trim());
  }
  return m;
}

// 合并登录 cookie 与新鲜 ttwid：ttwid 是 WS 握手必需，不能被登录 cookie 顶掉
function buildCookie(ttwid) {
  const m = COOKIE ? parseCookie(COOKIE) : new Map();
  if (ttwid && !m.has('ttwid')) m.set('ttwid', ttwid);
  return [...m].map(([k, v]) => `${k}=${v}`).join('; ');
}

function buildUrl(roomId, uid) {
  const internalExt = encodeURIComponent(
    `internal_src:dim|wss_push_room_id:${roomId}|wss_push_did:${uid}` +
      '|first_req_ms:0|fetch_time:0|seq:1|wss_info:0-0-0-0|wrds_v:0',
  );
  return (
    'wss://webcast3-ws-web-lf.douyin.com/webcast/im/push/v2/' +
    '?app_name=douyin_web&version_code=180800&webcast_sdk_version=1.3.0&update_version_code=1.3.0' +
    '&compress=gzip&live_id=1&aid=6383&did_rule=3&device_platform=web&identity=audience' +
    `&room_id=${roomId}&user_unique_id=${uid}` +
    '&cursor=d-1_u-1&host=https://live.douyin.com&im_path=/webcast/im/fetch/' +
    '&need_persist_msg_count=15&support_wrds=1' +
    `&internal_ext=${internalExt}`
  );
}

// saermart/DouyinLiveWebFetcher 的连接参数（用于对照：为什么它收得到礼物）
function buildUrlSaermart(roomId, uid) {
  const now = Date.now();
  const internalExt = encodeURIComponent(
    `internal_src:dim|wss_push_room_id:${roomId}|wss_push_did:${uid}` +
      `|first_req_ms:${now}|fetch_time:${now}|seq:1|wss_info:0-${now}-0-0|wrds_v:0`,
  );
  const browserVer = encodeURIComponent(
    '5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36',
  );
  return (
    'wss://webcast5-ws-web-lq.douyin.com/webcast/im/push/v2/?app_name=douyin_web' +
    '&version_code=180800&webcast_sdk_version=1.0.14-beta.0&update_version_code=1.0.14-beta.0' +
    '&compress=gzip&device_platform=web&cookie_enabled=true&screen_width=1536&screen_height=864' +
    '&browser_language=zh-CN&browser_platform=Win32&browser_name=Mozilla' +
    `&browser_version=${browserVer}` +
    '&browser_online=true&tz_name=Asia%2FShanghai' +
    `&cursor=d-1_u-1_fh-${roomId}_t-${now}_r-1` +
    `&internal_ext=${internalExt}` +
    '&host=https://live.douyin.com&aid=6383&live_id=1&did_rule=3&endpoint=live_pc&support_wrds=1' +
    `&user_unique_id=${uid}&im_path=%2Fwebcast%2Fim%2Ffetch%2F&identity=audience` +
    `&need_persist_msg_count=15&insert_task_id=&live_reason=&room_id=${roomId}&heartbeatDuration=0`
  );
}

function paramString(url) {
  const q = url.indexOf('?');
  const map = new Map();
  for (const kv of url.slice(q + 1).split('&')) {
    const i = kv.indexOf('=');
    if (i >= 0) map.set(kv.slice(0, i), decodeURIComponent(kv.slice(i + 1)));
  }
  return PARAM_ORDER.map((k) => `${k}=${map.get(k) ?? ''}`).join(',');
}

// ---------- 2. 手写 protobuf（varint + len-delimited） ----------
function readVarint(buf, p) {
  let v = 0n, shift = 0n;
  while (true) {
    const b = buf[p++];
    v |= BigInt(b & 0x7f) << shift;
    if ((b & 0x80) === 0) break;
    shift += 7n;
  }
  return [v, p];
}

function readFields(buf) {
  const out = [];
  let p = 0;
  while (p < buf.length) {
    let key;
    [key, p] = readVarint(buf, p);
    const num = Number(key >> 3n);
    const wire = Number(key & 7n);
    if (wire === 0) {
      let v;
      [v, p] = readVarint(buf, p);
      out.push({ num, wire, varint: v });
    } else if (wire === 2) {
      let len;
      [len, p] = readVarint(buf, p);
      const bytes = buf.subarray(p, p + Number(len));
      p += Number(len);
      out.push({ num, wire, bytes });
    } else if (wire === 5) {
      p += 4;
    } else if (wire === 1) {
      p += 8;
    } else {
      break; // 未知 wire type，停止
    }
  }
  return out;
}

const byNum = (fields, num) => fields.find((f) => f.num === num);
const asString = (f) => (f && f.bytes ? f.bytes.toString('utf8') : '');

// PushFrame: f2=seq_id, f3=log_id?（DanmuFree: f2=log_id, f8=payload gzip）—— 实测打印原始字段号再确认
function parsePushFrame(buf) {
  const fields = readFields(buf);
  const logId = byNum(fields, 2);
  const payload = byNum(fields, 8);
  if (!globalThis.__pfLogged) {
    globalThis.__pfLogged = true;
    console.log(`[帧] PushFrame 字段=${fields.map((f) => `${f.num}/${f.wire}`).join(',')} 长度=${buf.length}`);
  }
  return {
    fieldNums: fields.map((f) => `${f.num}/${f.wire}`).join(','),
    logId: logId ? logId.varint : 0n,
    payload: payload && payload.bytes ? payload.bytes : null,
  };
}

function parseResponse(buf) {
  const fields = readFields(buf);
  const needAck = byNum(fields, 9);
  const internalExt = asString(byNum(fields, 5));
  const messages = fields
    .filter((f) => f.num === 1 && f.bytes)
    .map((f) => {
      const mf = readFields(f.bytes);
      return { method: asString(byNum(mf, 1)), payload: byNum(mf, 2)?.bytes ?? null };
    });
  return { messages, internalExt, needAck: needAck ? needAck.varint !== 0n : false };
}

// 编码原语（log_id 是 uint64，必须用 BigInt，JS 位运算只有 32 位）
function encBig(n) {
  let v = BigInt(n);
  const b = [];
  do { b.push(Number(v & 0x7fn) | (v > 0x7fn ? 0x80 : 0)); v >>= 7n; } while (v > 0n);
  return Buffer.from(b);
}
const field = (num, wire, body) => Buffer.concat([encBig((BigInt(num) << 3n) | BigInt(wire)), body]);
const lenDelimited = (buf) => Buffer.concat([encBig(buf.length), buf]);
const strField = (num, s) => field(num, 2, lenDelimited(Buffer.from(s, 'utf8')));

function buildAck(logId, internalExt) {
  return Buffer.concat([
    field(2, 0, encBig(logId)),
    strField(7, 'ack'),
    strField(8, internalExt),
  ]);
}

// 心跳：空 PushFrame{f8 = gzip(空)}，f8 = 字段 8 wire 2 = 0x42
const EMPTY_GZIP_FRAME = field(8, 2, lenDelimited(zlib.gzipSync(Buffer.alloc(0))));
// saermart 的心跳：PushFrame{payload_type='hb'}（字段 7 wire 2 = 0x3a），走 PING 帧
const HB_PING_FRAME = field(7, 2, lenDelimited(Buffer.from('hb', 'utf8')));

// ---------- 3. 主流程 ----------
const seen = new Map();
let chatSamples = 0;
let frameCount = 0;
const dumpedMethods = new Set();
let gotGift = false;
const VERBOSE = process.env.PROBE_VERBOSE === '1';
const NO_ACK = process.env.NO_ACK === '1';
const T0 = Date.now();

async function main() {
  // 3.1 ttwid（服务端自动下发）
  const r1 = await fetch(`https://live.douyin.com/${RID}`, {
    headers: { 'user-agent': UA, referer: 'https://live.douyin.com/' },
  });
  const cookies = r1.headers.getSetCookie ? r1.headers.getSetCookie() : [];
  let ttwid = '';
  for (const c of cookies) {
    const m = /^ttwid=([^;]+)/.exec(c);
    if (m) ttwid = m[1];
  }
  console.log(`[1] 主页 status=${r1.status} ttwid=${ttwid ? ttwid.slice(0, 24) + '…' : '（空！）'}`);
  console.log(
    `[1] 登录态：${COOKIE ? `已提供（${cookieNames.length} 个 cookie：${cookieNames.join(',')}）` : '无（匿名）'}`,
  );
  await r1.arrayBuffer().catch(() => {});

  // 3.2 真实 room_id
  const enterUrl =
    'https://live.douyin.com/webcast/room/web/enter/' +
    '?aid=6383&app_name=douyin_web&live_id=1&device_platform=web&language=zh-CN' +
    '&cookie_enabled=true&screen_width=1920&screen_height=1080&browser_language=zh-CN' +
    '&browser_platform=Win32&browser_name=Chrome&browser_version=126.0.0.0' +
    `&web_rid=${RID}&enter_from=web_live&is_need_double_stream=false`;
  const r2 = await fetch(enterUrl, {
    headers: {
      'user-agent': UA,
      referer: `https://live.douyin.com/${RID}`,
      'accept-language': 'zh-CN,zh;q=0.9',
      cookie: buildCookie(ttwid),
    },
  });
  const json = await r2.text();
  const m = /"id_str"\s*:\s*"(\d{15,25})"/.exec(json);
  const roomId = m ? m[1] : RID;
  console.log(`[2] enter status=${r2.status} bodyLen=${json.length} room_id=${roomId}${m ? '' : '（正则没命中！）'}`);
  if (!m) {
    fs.writeFileSync(path.join(__dirname, 'enter-dump.json'), json);
    console.log('    已 dump 到 probe/node/enter-dump.json');
  }

  // 3.3 签名
  const uid = String(Math.floor(Math.random() * 9) + 1) + String(Date.now()).padStart(10, '0') + String(Math.floor(Math.random() * 100000)).padStart(5, '0');
  const url = (VARIANT === 'saermart' ? buildUrlSaermart : buildUrl)(roomId, uid);
  console.log(`[3] 连接参数变体 = ${VARIANT}`);
  const param = paramString(url);
  const md5 = crypto.createHash('md5').update(param, 'utf8').digest('hex');
  console.log(`[3] user_unique_id=${uid}`);
  console.log(`[3] paramString=${param}`);
  console.log(`[3] X-MS-STUB(md5)=${md5}`);
  const t0 = Date.now();
  const sig = getXbogus(md5);
  console.log(`[3] X-Bogus=${sig}  (耗时 ${Date.now() - t0} ms)`);
  if (!sig) { console.log('签名失败，终止'); process.exit(1); }

  // 3.4 连 WS
  const full = url + '&signature=' + encodeURIComponent(sig);
  const ws = new WebSocket(full, {
    headers: {
      'User-Agent': UA,
      Origin: 'https://live.douyin.com',
      Cookie: process.env.COOKIE_WS === '0' ? `ttwid=${ttwid}` : buildCookie(ttwid),
    },
  });

  ws.on('unexpected-response', (req, res) => {
    const hs = JSON.stringify(res.headers);
    console.log(`[4] ❌ 握手被拒 HTTP ${res.statusCode} headers=${hs}`);
    res.resume();
  });
  ws.on('upgrade', (res) => {
    console.log(`[4] 握手 HTTP ${res.statusCode}${res.statusCode === 101 ? ' ✅ 签名被接受' : ' ❌ ' + JSON.stringify(res.headers)}`);
  });
  ws.on('open', () => {
  console.log(`[4] WS 已打开，开始收帧（最多 ${DURATION_SEC} 秒 / 收到弹幕即退出；NO_ACK=${NO_ACK} VERBOSE=${VERBOSE}）`);
    const hb = setInterval(() => {
      if (VARIANT === 'saermart') ws.ping(HB_PING_FRAME);
      else ws.send(EMPTY_GZIP_FRAME, { binary: true });
    }, 10000);
    ws.on('close', () => clearInterval(hb));
    setTimeout(() => { console.log(`[!] ${DURATION_SEC} 秒到，主动结束`); ws.close(); }, DURATION_SEC * 1000);
  });
  ws.on('error', (e) => console.log('[!] WS 错误：' + e.message));
  ws.on('close', (code, reason) => {
    console.log(`[5] 关闭 code=${code} reason=${reason}`);
    console.log(`[5] 共收到 method 种类：${[...seen.entries()].map(([k, v]) => `${k}×${v}`).join(', ') || '（无）'}`);
    process.exit(0);
  });

  ws.on('message', (data) => {
    frameCount++;
    const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
    let pf;
    try { pf = parsePushFrame(buf); } catch (e) { console.log('[!] PushFrame 解析失败：' + e.message); return; }
    if (!pf.payload) { console.log(`[帧] 无 f8 payload，字段=${pf.fieldNums}`); return; }

    // payload 可能是 gzip；直接找 gzip magic 解压
    let raw = pf.payload;
    const magic = raw.indexOf(Buffer.from([0x1f, 0x8b]));
    if (magic >= 0) {
      try { raw = zlib.gunzipSync(raw.subarray(magic)); } catch (e) { console.log('[!] gunzip 失败：' + e.message); return; }
    }
    let resp;
    try { resp = parseResponse(raw); } catch (e) { console.log('[!] Response 解析失败：' + e.message); return; }

    for (const msg of resp.messages) {
      if (!msg.method) continue;
      seen.set(msg.method, (seen.get(msg.method) ?? 0) + 1);
      const detail = decodeDetail(msg);
      console.log(`[消息] ${msg.method} ${detail}`);
      // PROBE_USER=1：逐条打印用户身份字段，用来分清 f6 / PayGrade / 灯牌
      if (process.env.PROBE_USER === '1') {
        const uf = readFields(msg.payload);
        const userBytes =
          msg.method === 'WebcastGiftMessage' ? byNum(uf, 7)?.bytes : byNum(uf, 2)?.bytes;
        if (userBytes && ['WebcastChatMessage', 'WebcastMemberMessage', 'WebcastSocialMessage', 'WebcastLikeMessage', 'WebcastGiftMessage'].includes(msg.method)) {
          console.log(`[user] ${userSummary(userBytes)}`);
          maybeDumpFansClub(userBytes);
        }
      }
      if (msg.method === 'WebcastChatMessage') chatSamples++;
      if (msg.method === 'WebcastGiftMessage') {
        console.log('[礼物 payload 字段 dump]');
        console.log(dumpFields(msg.payload));
        gotGift = true;
      }
      if (
        !dumpedMethods.has(msg.method) &&
        ['WebcastChatMessage', 'WebcastMemberMessage', 'WebcastRoomUserSeqMessage', 'WebcastLikeMessage'].includes(msg.method)
      ) {
        dumpedMethods.add(msg.method);
        console.log(`[${msg.method} 字段 dump]`);
        console.log(dumpFields(msg.payload));
      }
    }
    if (VERBOSE) {
      console.log(
        `[帧 #${frameCount} +${Date.now() - T0}ms] needAck=${resp.needAck} 消息数=${resp.messages.length} ` +
          `logId=${pf.logId} internal_ext=${resp.internalExt.slice(0, 80)}`,
      );
    }
    if (resp.needAck && !NO_ACK) {
      const ack = buildAck(pf.logId, resp.internalExt);
      if (VERBOSE) console.log(`[ack] ${ack.length} 字节 ${ack.toString('hex').slice(0, 120)}`);
      try { ws.send(ack, { binary: true }); } catch (e) { console.log('[!] ack 发送失败：' + e.message); }
    }
    if (gotGift) { console.log('[✓] 抓到礼物，2 秒后结束'); setTimeout(() => ws.close(), 2000); return; }
    const chatStop = Number(process.env.CHAT_STOP || 3);
    if (chatSamples >= chatStop) { console.log(`[✓] 已收到 ${chatStop} 条弹幕，主动结束`); ws.close(); }
  });
}

// 把一个 protobuf payload 逐层展开，用来确定字段号（礼物名/数量等需要实测）
function dumpFields(buf, depth = 0, maxDepth = 5) {
  const pad = '  '.repeat(depth);
  let fields;
  try { fields = readFields(buf); } catch { return pad + '(解析失败)'; }
  const lines = [];
  for (const f of fields) {
    if (f.wire === 0) { lines.push(`${pad}f${f.num}/varint = ${f.varint}`); continue; }
    if (!f.bytes) continue;
    const s = f.bytes.toString('utf8');
    // eslint-disable-next-line no-control-regex
    const printable = s.length > 0 && !/[\x00-\x08\x0e-\x1f\ufffd]/.test(s);
    if (printable) lines.push(`${pad}f${f.num}/str = ${JSON.stringify(s.slice(0, 80))}`);
    else {
      lines.push(`${pad}f${f.num}/msg(len=${f.bytes.length})`);
      if (depth + 1 < maxDepth) lines.push(dumpFields(f.bytes, depth + 1, maxDepth));
    }
  }
  return lines.join('\n');
}

// 用户身份字段的紧凑摘要（PROBE_USER=1）：用来分清「用户等级 / 粉丝团灯牌 / f6 到底是什么」
// User: f1=id, f3=nickName, f6=Level(?), f23=PayGrade(f6=等级 varint、f19=等级图标),
//       f24=FansClub{ f1=FansClubData{ f1=clubName, f2=level } }
function userSummary(userBytes) {
  const uf = readFields(userBytes);
  const nick = asString(byNum(uf, 3));
  const f6 = byNum(uf, 6)?.varint;
  const pg = byNum(uf, 23)?.bytes ? readFields(byNum(uf, 23).bytes) : null;
  const pgLevel = pg ? byNum(pg, 6)?.varint : undefined;
  const fc = byNum(uf, 24)?.bytes ? readFields(byNum(uf, 24).bytes) : null;
  let club = '';
  if (fc) {
    const data = byNum(fc, 1)?.bytes ? readFields(byNum(fc, 1).bytes) : null;
    if (data) club = `${asString(byNum(data, 1))}(lv${byNum(data, 2)?.varint ?? '?'})`;
  }
  // f19/f21 里的等级图标（new_user_grade_level_v1_N.png）也挖一下
  let iconLevel = '';
  const m = /new_user_grade_level_v\d+_(\d+)\.png/.exec(userBytes.toString('latin1'));
  if (m) iconLevel = m[1];
  return `nick=${nick} f6=${f6 ?? '-'} payGrade=${pgLevel ?? '-'} 图标等级=${iconLevel || '-'} 灯牌=${club || '-'} f24len=${byNum(uf, 24)?.bytes ? byNum(uf, 24).bytes.length : '-'}`;
}

// 灯牌（f24）带名字与等级，结构只看一次就够（PROBE_USER=1 且首次遇到 len>100 时输出）
let dumpedFansClub = false;
function maybeDumpFansClub(userBytes) {
  if (dumpedFansClub) return;
  const uf = readFields(userBytes);
  const fc = byNum(uf, 24)?.bytes;
  if (!fc || fc.length < 100) return;
  dumpedFansClub = true;
  console.log('[fansclub dump] User.f24 →\n' + dumpFields(fc));
}

// 按 method 抠出关键字段（字段号待实测确认）
function decodeDetail(msg) {
  try {
    const f = readFields(msg.payload);
    if (msg.method === 'WebcastChatMessage') {
      const user = byNum(f, 2)?.bytes;
      const content = asString(byNum(f, 3));
      const nick = user ? asString(byNum(readFields(user), 3)) : '';
      return `nick=${nick} content=${content}`;
    }
    if (msg.method === 'WebcastMemberMessage' || msg.method === 'WebcastSocialMessage') {
      const user = byNum(f, 2)?.bytes;
      const nick = user ? asString(byNum(readFields(user), 3)) : '';
      return `nick=${nick} fields=${f.map((x) => x.num).join(',')}`;
    }
    // GiftMessage: f6=combo_count, f7=user, f15=GiftStruct; GiftStruct: f12=diamondCount, f16=name
    if (msg.method === 'WebcastGiftMessage') {
      const user = byNum(f, 7)?.bytes;
      const nick = user ? asString(byNum(readFields(user), 3)) : '';
      const gift = byNum(f, 15)?.bytes;
      const gf = gift ? readFields(gift) : [];
      return (
        `nick=${nick} gift=${asString(byNum(gf, 16))} giftId=${byNum(gf, 5)?.varint ?? byNum(f, 2)?.varint} ` +
        `diamond=${byNum(gf, 12)?.varint ?? 0} combo=${byNum(f, 6)?.varint ?? 0} repeat=${byNum(f, 5)?.varint ?? 0}`
      );
    }
    if (msg.method === 'WebcastRoomUserSeqMessage') {
      return `total(在线)=${byNum(f, 3)?.varint} totalUser=${byNum(f, 7)?.varint} totalStr=${asString(byNum(f, 9))}`;
    }
    return `fields=${f.map((x) => `${x.num}/${x.wire}`).join(',')}`;
  } catch (e) {
    return '（解析异常 ' + e.message + '）';
  }
}

main().catch((e) => { console.error('探针异常：', e); process.exit(1); });
