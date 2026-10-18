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
| 账号观看会话 | 同一账号 + 同一直播间**只保留一个**：手机端 / 网页端 / 助手会互相顶（提示「账号已在其他地方进入直播间」） |

### 账号观看会话（实测）

抖音按「账号 + 直播间」只保留一个观看会话，这是平台规则而非本项目的问题（手机端与网页端进同一直播间本来就互相顶）。

本项目踩到的坑：`enter` 请求里带了登录 Cookie，等于告诉抖音「账号从助手进入直播间」，于是**一连接就把网页端顶掉**。

| 请求 | 带登录 Cookie | 不带（只 `ttwid`） |
| --- | --- | --- |
| `enter`（进入直播间注册） | 网页端被顶 | 不影响别人，`room_id` / 主播名 / 开播状态一样给 |
| 弹幕 WS 握手 | 不影响别人（只是消息通道，不参与观看会话竞争），礼物消息只在登录态下推 | 收不到礼物 |

另有两条对照实验：带登录 Cookie 的 `enter` 不会踢掉已建立的 WS；同一账号两条登录态 WS 也能共存。

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

## 发送弹幕（已实测，**结论留档：不做**）

不做这个功能的原因：唯一可行的实现是把**整个直播间页**塞进隐藏窗口（见下），内存开销明显；而目标用户是主播与一起玩的水友，本来就开着语音，用不上弹幕。

| 环节 | 结论 |
| --- | --- |
| 接口 | `GET https://live.douyin.com/webcast/room/chat/`，**参数全在 query**，没有 body（不是 POST） |
| 成功判据 | **不能只看 `status_code`**：缺 `a_bogus` 时服务端照样回 `status_code: 0`，但**根本不广播** —— 房间里看不到，连自己订阅的弹幕窗也收不到。判据只能是「房间里真出现」 |
| 签名 | **`a_bogus` 必需**，且只能由 webmssdk 生成：删掉它 → `20003`；带一个与 query 不匹配的旧签名 → 同样被拒。仓库里的 `sign/sign.js` 只导出 X-Bogus，生成不了它 |
| msToken | **必需**：缺了回 `20003`（文案是 `User doesn't login`，会误导成登录问题）。可从任意 `enter` 响应头的 `X-Ms-Token` 取 |
| 参数指纹 | `browser_name` / `browser_version` / 屏幕尺寸要与发请求的环境一致（WebView2 是 Edge，写死 Chrome 会被判异常），所以该由页面用 `navigator` 自己拼 |
| 唯一可行做法 | 隐藏窗口加载 `https://live.douyin.com/{短号}`，用**页面自己的** `XMLHttpRequest` 发请求 —— 页面里的 webmssdk 会 hook XHR 自动补 `a_bogus`（实测可行）。代价：整个直播页真被加载（即使暂停了播放器） |
| 回包通道 | 该页面上 `__TAURI_INTERNALS__` 调不通、`document.title` 不会同步到窗口标题（两条都实测失败）；能用的是 **cookie**（页面写、Rust 用 `WebviewWindow::cookies()` 读） |
| 错误码 | `20003` = 未登录 / 风控 / 缺 token（文案分不出）；`50007` = 房间级拒绝（实测：同一房间用浏览器能发，但刚跨房间连发就被回 50007） |

> 踩坑：`status_code: 0` 一开始骗过了我们 —— 只看响应会以为「去掉 `a_bogus` 也能发」，实际服务端只是**假成功**。换成「房间里有没有真出现」当判据才看清。

## 关注在播列表（已实测，**不需要签名**）

www 关注页「正在直播(N)」那一块的接口，一个请求就能拿到「已关注且在播」的全部主播。

| 环节 | 结论 |
| --- | --- |
| 接口 | `POST https://www.douyin.com/webcast/feed/follow_top/?device_platform=webapp&aid=6383&channel=channel_pc_web`，参数在 **body**（`application/x-www-form-urlencoded`） |
| 必填 body | `source_key=web_homepage_follow_top` + `follow_session_id=<游标>`（首次传 0）。页面还会带 `enter_source` / `need_map` / `need_pinned_info`，实测可省 |
| 签名 | **不需要** `a_bogus` / `msToken` / `uifid` / `x-secsdk-csrf-token`（全部删掉仍回 `status_code: 0`）；只需登录 Cookie |
| 返回 | `data[]` 每条一个直播间：`owner.nickname` 主播名、`web_rid` **直播间短号**（不是抖音号）、`data.id_str` 真实 room_id |
| 翻页 | `extra.follow_session_id`（下一页游标）+ `extra.has_more`；`extra.total` 是在播总数 |

> 踩坑：参数只放 query 里会回 `status_code=10011 Request params error`，很容易误判成「缺签名」——真凶是 body 里的 `source_key`。

### 废弃的两步方案（留档）

先试过「关注列表 `GET /webcast/user/follow/list/` + 逐个 uid 探活 `GET /webcast/distribution/check_user_live_status/`」：

- 关注列表只给 uid / 昵称 / 抖音号，**不含开播状态**（`web_rid` 字段实测为空）
- 探活接口**一次只认第一个 uid**（`user_ids=A+B` 只看 A；逗号、重复参数直接报 `10011`），关注 500 人就得打 500 个请求

→ 已被 `follow_top` 取代。

## 「我的直播间」（自己 + 别人的主页，已实测）

主界面「关注在播」首位那条固定文案「我的直播间」，靠下面这组实测拼出来（都是临时 PowerShell 探针直接 GET，未留脚本）。

### 自己是谁：`profile/self`

| 环节 | 结论 |
| --- | --- |
| 接口 | `GET https://www.douyin.com/aweme/v1/web/user/profile/self/?device_platform=webapp&aid=6383&channel=channel_pc_web`，带登录 Cookie，**不需要签名**（`a_bogus` / `msToken` 都不给也回 `status_code: 0`） |
| 返回字段 | `user.uid`（**字符串**形态）/ `user.nickname` / `user.unique_id`（= **抖音号**）/ `user.short_id` / `user.live_status` / `user.room_id` / `user.room_id_str`；未登录回 `status_code: 8`「用户未登录」 |
| 别人同款 | 换 `profile/other/?sec_user_id=<sec_uid>`（就是个人主页地址那串）字段一模一样，而且**匿名（只带 ttwid）也能查**：开播中的主播 `live_status=1`、`room_id_str` 就是真实 19 位 room_id（实测 十叁哥 → `7697605570416659235`） |

### `room_id_str` 当不了「有没有房间」的判据（实测）

| 账号 | 实际情况 | `live_status` / `room_id_str` |
| --- | --- | --- |
| 冰冻大西瓜 | 从未开播，无房间 | `0` / `"0"` |
| 贱贱 | 有房间（`enter` 仍认得出旧房间 `7697599161746983716`，`status=4` 已结束），但已下播 | `0` / `"0"` |

→ **未开播就归 0**（房间对象还在也没用）。拿它当门槛反而会把「开播前先连上自己房间」这个主场景挡掉，所以这一条**不看开播状态**，房间真不存在时点击就走 `resolver` 的正常报错（「没有解析出房间号…」）。这一点与 B 站相反：B 站 `get_status_info_by_uids` 对未开播的 uid 照样给真实 `room_id`。

### 拿去连接的「号」：抖音号

| 结论 | 实测 |
| --- | --- |
| `enter` **认抖音号** | `enter&web_rid=45364297796` 的 `data.user.id_str` 与 `profile/self` 的 `uid` 一致（账号 冰冻大西瓜）；`enter&web_rid=62876576864` 认出 贱贱（那是他的 `unique_id`） |
| `enter` 也认直播间短号 | 同一账号用真 `web_rid`（`follow_top` 给的 `639280210755`）与抖音号两种写法返回同一个 room |
| `enter` **不认 uid** | `web_rid=1720053578867822` 回 `status_code=4001038` |
| `enter` **不认 room_id** | `web_rid=7697605570416659235`（真实房间号）同样回 `4001038` —— 所以拿到 `room_id_str` 也塞不进「短号 → room_id」那条流程 |
| 标识不固定在哪一个字段 | 贱贱 `unique_id=62876576864` / `short_id=0`；十叁哥反过来（字母 `unique_id=88866kzi` / `short_id=0`）—— 取标识得「两个里挑数字/字母的那个」 |
| 抖音号可以是**字母** | 十叁哥 `unique_id=88866kzi`：`enter&web_rid=88866kzi` 正常返回他的房间，`live.douyin.com/88866kzi` 也是 HTTP 200 + 带 ttwid（resolver 第 1 步没问题）。所以 `connect_room` 与输入框的校验是**数字 / 字母都收**；带 `_` 这类符号的抖音号不收 |

所以「我的直播间」= 抖音号（`unique_id`，空了用 `short_id`）当短号，走项目既有的「短号 → room_id」流程 —— 用户手输抖音号也一样能连。

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
