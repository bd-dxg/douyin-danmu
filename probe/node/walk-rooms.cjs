const UA =
  'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36'
async function enter(rid, ttwid) {
  const url =
    'https://live.douyin.com/webcast/room/web/enter/?aid=6383&app_name=douyin_web&live_id=1&device_platform=web&language=zh-CN&cookie_enabled=true&screen_width=1920&screen_height=1080&browser_language=zh-CN&browser_platform=Win32&browser_name=Chrome&browser_version=126.0.0.0&web_rid=' +
    rid +
    '&enter_from=web_live&is_need_double_stream=false'
  const r = await fetch(url, {
    headers: { 'user-agent': UA, referer: `https://live.douyin.com/${rid}`, cookie: `ttwid=${ttwid}` },
  })
  const j = await r.json()
  const d = j.data || {}
  const room = (d.data || [])[0] || {}
  return {
    status: room.status,
    count: room.user_count_str,
    nick: d.user && d.user.nickname,
    roomId: room.id_str,
    sim: (d.similar_rooms || []).map(x => x.web_rid).filter(Boolean),
  }
}
;(async () => {
  const r1 = await fetch('https://live.douyin.com/369324308707', {
    headers: { 'user-agent': UA, referer: 'https://live.douyin.com/' },
  })
  const sc = r1.headers.getSetCookie ? r1.headers.getSetCookie() : []
  let ttwid = ''
  for (const c of sc) {
    const m = /^ttwid=([^;]+)/.exec(c)
    if (m) ttwid = m[1]
  }
  await r1.arrayBuffer().catch(() => {})
  const base = await enter('369324308707', ttwid)
  const seen = new Set()
  const queue = [...base.sim].slice(0, 10)
  const live = []
  for (const rid of queue) {
    if (seen.has(rid)) continue
    seen.add(rid)
    try {
      const info = await enter(rid, ttwid)
      const line = `${rid} status=${info.status} 在线=${info.count} 主播=${info.nick}`
      console.log(line)
      if (String(info.status) === '2') live.push({ rid, count: Number(info.count) || 0 })
    } catch (e) {
      console.log(rid, '失败', e.message)
    }
  }
  live.sort((a, b) => b.count - a.count)
  console.log('=== 直播中的房间（按在线排序）===')
  console.log(
    live
      .slice(0, 5)
      .map(x => `${x.rid}(${x.count})`)
      .join(' '),
  )
})()
