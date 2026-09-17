# 抖音弹幕协议探针（probe/）

一次性用途：在不搭 Tauri 工程的前提下，把抖音弹幕协议的关键环节实测一遍。**结论已全部验证**，实施时照这份文档写 Rust，不用重新逆向。

## 已实测确认的事实

| 环节 | 结论 |
| --- | --- |
| ttwid | `GET https://live.douyin.com/{短号}`（UA=Chrome 126 + `Referer: https://live.douyin.com/`）的 Set-Cookie 自动下发。**匿名可用，不需要登录** |
| 真实 room_id | `GET https://live.douyin.com/webcast/room/web/enter/?...&web_rid={短号}`，带 `Cookie: ttwid=...`，正则 `"id_str"\s*:\s*"(\d{15,25})"`。**不需要 a_bogus**；短号不能直接连 WS |
| 签名 X-Bogus | 13 字段按固定顺序拼 `k=v,k=v,...` → `MD5` 小写 hex（=X-MS-STUB）→ webmssdk `getSign` → X-Bogus。**同一 md5 每次结果不同**（掺环境指纹），服务端都认 |
| 签名运行环境 | Node+jsdom 可以；**真浏览器（无 jsdom）也可以** —— sign.js 前 7 行是 jsdom 补环境，去掉后直接用真 window 就能跑（见 `browser/`） |
| WS 域名 | `wss://webcast3-ws-web-lf.douyin.com/webcast/im/push/v2/`，**必须带 `-ws-web-`** |
| 握手 | HTTP 101 = 签名被接受；200 + `Handshake-Msg: auth failed` = 签名错 |
| PushFrame | 实测字段 `1,2,3,4,5(×5),6,7,8`；`f2`=log_id(varint)、`f8`=payload(gzip) |
| Response | `f1`=messages(repeated)、`f5`=internal_ext(string)、`f9`=need_ack(bool) |
| Message | `f1`=method(string)、`f2`=payload(bytes) |
| ack | 回 `PushFrame{f2=log_id, f7="ack", f8=internal_ext}`。**不回 → 服务端每 1–2 秒重推同一帧（cursor 不推进）；回了 → 推进、不重复** |
| internal_ext | 服务端回的形如 `internal_src:pushserver\|first_req_ms:...\|seq:1\|wss_msg_type:wrds\|wrds_...` |

## 字段号权威表（`probe/reference/douyin.proto` + 实测）

来自 saermart/DouyinLiveWebFetcher 的 `protobuf/douyin.proto`，**已下载到 `reference/`**；标记 ✅ 的是本机实测确认过的。

| 消息 | 字段 |
| --- | --- |
| **PushFrame** | f1=seqId, `f2=logId` ✅, f3=service, f4=method, f5=headers(repeated), f6=payloadEncoding, f7=payloadType, `f8=payload(gzip)` ✅ |
| **Response** | `f1=messages(repeated)` ✅, f2=cursor, f3=fetchInterval, `f5=internalExt` ✅, `f9=needAck` ✅ |
| **Message** | `f1=method` ✅, `f2=payload` ✅, f3=msgId |
| **ChatMessage**（弹幕） | f1=Common, `f2=User` ✅, `f3=content` ✅, f15=eventTime |
| **User** | f1=id, `f3=nickName` ✅, f4=gender, f6=Level, f21=BadgeImageList, f24=FansClub |
| **MemberMessage**（进房） | f1=Common, `f2=User` ✅, f3=memberCount, f9=enterType, f15=enterEffectConfig |
| **SocialMessage**（关注/分享） | f1=Common, `f2=User` ✅, f3=shareType, f4=action, f6=followCount |
| **LikeMessage**（点赞） | f2=count, f3=total, f5=User |
| **RoomUserSeqMessage**（在线数） | `f3=total(在线)` ✅, f7=totalUser, f8=totalUserStr, f9=totalStr, f10=onlineUserForAnchor, f11=totalPvForAnchor |
| **GiftMessage**（礼物） | f2=giftId, f4=groupCount, f5=repeatCount, **f6=comboCount**, **f7=User**, f9=repeatEnd, f15=GiftStruct |
| **GiftStruct** | f2=describe, f5=id, **f12=diamondCount（价值）**, **f16=name（礼物名）** |
| **Common** | f1=method, f2=msgId, f3=roomId, f4=createTime, f7=describe, f12=logId |

> 礼物价格用 `GiftStruct.diamondCount`（钻石），**但没有真实礼物样本落地验证过**（测试房间 3 分钟没人送礼），这一条是照 proto 推的。

## 礼物消息需要登录态（已实测确认）

| 条件 | 房间 `765790908625` 实测 |
| --- | --- |
| 匿名 | 180 秒：`WebcastChatMessage` 114 / `WebcastMemberMessage` 224 / `WebcastLikeMessage` 62，**`WebcastGiftMessage` 0 条** |
| 带登录 cookie | 120 秒：**`WebcastGiftMessage` 8 条**，解码正常 |

实测样本（字段号照 proto 推的全对）：

```
nick=未来之城          gift=粉丝团灯牌 giftId=685 diamond=1 combo=1 repeat=1
nick=用户3077822122717 gift=小心心    giftId=463 diamond=1 combo=1 repeat=1
```

结论：**弹幕/进场/关注/点赞/在线数匿名即可，礼物必须登录态**。所以「能连上」不代表「消息全」。

### 两个坑

1. **WS 握手必须带 `ttwid`**：用登录 cookie 整串替换掉 ttwid → `HTTP 200` + `handshake-msg: http: named cookie not present` + `handshake-status: 417`。要**合并**（ttwid 必留），见 `buildCookie()`。
2. **不是连接参数的问题**：把整套参数换成 saermart 的（`webcast5-ws-web-lq` 域名、`endpoint=live_pc`、真实时间戳的 `internal_ext`、PING 心跳）结果完全相同。

### 上游佐证

- [f2 issue #428](https://github.com/Johnserf-Seed/f2/issues/428)「抖音直播 WebcastGiftMessage 抓取礼物信息失效」，现象与本机匿名时一致
- [jwwsjlm/douyinLive](https://github.com/jwwsjlm/douyinLive) README 声明「礼物消息缺失…不作为适配目标」
- CSDN 上另一份解析记录提到抖音弹幕接口的三个关键凭证 `msToken` / `odin_tt` / `sessionid`

### 对 App 的影响

抖音礼物既然要登录，App 就绕不开登录流程。好消息：签名本来就要用隐藏 WebView2，**同一个隐藏窗口可以兼做扫码登录**（登录后从 WebView2 里读 `ttwid` + `sessionid` 等 cookie）。

## 怎么复跑

```powershell
cd probe\node
pnpm install
node ws-probe.cjs <直播间短号> [秒数]      # 默认 256438100956 / 30 秒
$env:PROBE_VERBOSE='1'; node ws-probe.cjs <短号> 40   # 打印每帧 need_ack / internal_ext / ack 十六进制
$env:NO_ACK='1';        node ws-probe.cjs <短号> 40   # 对照：不回 ack，观察重推
$env:CHAT_STOP='1';     node ws-probe.cjs <短号> 60   # 收到 1 条弹幕就退出（快速抓字段样本用）
node walk-rooms.cjs <任意直播间短号>   # 沿 similar_rooms 找出正在直播的房间（按在线人数排序），用来抽样本
```

浏览器侧（验证不依赖 jsdom）：
```powershell
& 'C:\Program Files\Google\Chrome\Application\chrome.exe' --headless=new --disable-gpu --dump-dom `
  "file:///<仓库绝对路径>/probe/browser/poc.html"
# 输出里的 <pre> 应含 {"X-Bogus":"..."}
```

`browser/sign_browser.js` 是 `sign/sign.js` 去掉前 7 行 jsdom 补环境后的产物，由 `probe/browser/` 下一条命令生成（切片脚本见 git 历史/本文档描述）。

### 从零准备签名 JS（新克隆的仓库）

`sign/sign.js`、`browser/sign_browser.js`、`reference/` **都不进仓库**（第三方代码/产物，授权口径与本项目的 GPL-3.0 不一致，见 `.gitignore`），需要时自己拉一次：

```powershell
$dest = 'probe\sign\sign.js'
curl.exe -L -o $dest 'https://raw.githubusercontent.com/SoraYjy/DanmuFree/main/sign/sign.js'
# 切掉前 7 行（jsdom 补环境），剩下的直接用真浏览器 window 就能跑
(Get-Content $dest | Select-Object -Skip 7) -join "`n" | Set-Content probe\browser\sign_browser.js -Encoding utf8
# 再拷成前端静态资源（应用运行时加载的就是这个）
pwsh -File scripts\copy-sign-js.ps1
```

`reference/douyin.proto`、`reference/liveMan.py` 来自 [saermart/DouyinLiveWebFetcher](https://github.com/saermart/DouyinLiveWebFetcher)（AGPL-3.0，仅在本地作字段号对照，不进仓库）。

## 还没验证的

- 礼物价值换算：`diamondCount` 与人民币的对应关系（1 钻石 = ？元），未核对
- 带登录态时其他消息类是否变化（目前看弹幕/进场/关注/点赞/在线数在两种情况下都有）
- 长时间（>10 分钟）稳定性与断线重连行为
- `repeat_end(f9)` 对连击结束的语义
- 登录 cookie 的获取方式（扫码登录后在隐藏 WebView2 里读，尚未实现）

## 依赖说明

探针用 Node 只是为了快速验证，**正式实现不需要 Node**：签名走 Tauri 自带的隐藏 WebView2（加载 `sign_browser.js`），其余协议层全在 Rust。
