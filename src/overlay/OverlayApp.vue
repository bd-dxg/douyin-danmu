<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'

import {
  DEFAULT_DANMAKU_FILTER,
  DEFAULT_GIFT_CONFIG,
  DEFAULT_OVERLAY_STYLE,
  type BackingEvent,
  type DanmakuEvent,
  type DanmakuFilter,
  type DisplayDanmaku,
  type GiftConfig,
  type OverlayStyle,
  type RoomStatusEvent,
  type WelcomeConfig,
  type WelcomeEvent,
} from '../types/ipc'
import DanmakuRow from './DanmakuRow.vue'
import GiftRow from './GiftRow.vue'

const danmakuList = ref<DisplayDanmaku[]>([])
// 礼物列表：Rust 侧已做完连击合并与金额门槛，这里只负责按 id 覆盖与条数上限
const giftList = ref<BackingEvent[]>([])
const giftCfg = ref<GiftConfig>({ ...DEFAULT_GIFT_CONFIG })
// 当前直播间短号（仅用于系统提示行）
const currentRoom = ref('')
// 连接状态（空窗时显示初始化提示，避免无界面窗口）
const connState = ref<'disconnected' | 'connected'>('disconnected')
// 弹幕过滤配置（设置页变更 → Rust 广播 danmaku-filter 事件 → 实时生效）
const filter = ref<DanmakuFilter>({ ...DEFAULT_DANMAKU_FILTER })
// 弹幕样式（字号/字体/是否用原色）；初值用共用默认值，挂载后从 Rust 拉真实配置
const style = ref<OverlayStyle>({ ...DEFAULT_OVERLAY_STYLE })

// 弹幕区 / 礼物区共用的半透明黑（背景不透明度 0-100 → alpha 0-1）
const bgColor = computed(() => `rgba(0, 0, 0, ${(style.value.bg_opacity / 100).toFixed(2)})`)

const MAX_ITEMS = 120

/** 连接状态 → 界面状态（事件与轮询共用，去重） */
function applyConnStatus(st: RoomStatusEvent) {
  if (st.state === 'connected') {
    const was = connState.value === 'connected'
    const prevRoom = currentRoom.value
    connState.value = 'connected'
    currentRoom.value = st.webRid
    // 进入（或换房）时：清空旧提示并以系统行提示入场
    if (!was || prevRoom !== st.webRid) {
      danmakuList.value = []
      giftList.value = []
      pushSystem(`已进入直播间 ${st.webRid}，等待弹幕…`)
    }
  } else if (st.state === 'connecting') {
    if (connState.value !== 'connecting') {
      connState.value = 'connecting'
      danmakuList.value = []
      giftList.value = []
      pushSystem('连接中…')
    }
  } else {
    if (connState.value !== 'disconnected') {
      connState.value = 'disconnected'
      danmakuList.value = []
      giftList.value = []
      pushSystem('未连接 · 请在主界面连接直播间')
    }
  }
}

/** 追加一行并裁到上限（弹幕 / 系统提示 / 欢迎信息共用同一份列表与上限） */
function pushRow(row: DisplayDanmaku) {
  const list = danmakuList.value
  list.push(row)
  if (list.length > MAX_ITEMS) {
    list.splice(0, list.length - MAX_ITEMS)
  }
}

/** 推送系统提示行（无用户名，纯文本，与弹幕同样式） */
function pushSystem(text: string) {
  pushRow({
    id: `sys-${Date.now()}-${danmakuList.value.length}`,
    username: '',
    content: text,
    timestamp: Date.now() / 1000,
  })
}

/** 追加一条弹幕，超出上限丢弃最旧的 */
function pushDanmaku(d: DisplayDanmaku) {
  pushRow(d)
}

/**
 * 欢迎信息文案：`▸` 前缀让它在弹幕里一眼可辨
 *
 * 文字颜色改成与弹幕同亮度的淡青后，层次靠色相；再给个前缀符号，
 * 即使有人把弹幕正文色也改成青色也能分得出来。
 * 抖音只有进房 / 关注 / 点赞三类（没有舰队）。
 */
function welcomeText(w: WelcomeEvent): string {
  switch (w.kind) {
    case 'enter':
      return `▸ ${w.username} 进入直播间`
    case 'follow':
      return `▸ ${w.username} 关注了直播间`
    case 'like':
      return `▸ ${w.username} 点赞了直播间`
  }
}

/**
 * 推送一条欢迎提示行（无用户名、不显示徽章，与系统提示行同一形态）
 *
 * 去重与限速都在 Rust 侧做完（`welcome.rs`），这里只渲染收到的行；
 * 开关关掉时 Rust 直接不广播，所以无需在前端再判一次配置。
 */
function pushWelcome(w: WelcomeEvent) {
  pushRow({
    id: w.id,
    username: '',
    content: welcomeText(w),
    timestamp: w.timestamp,
    isWelcome: true,
  })
}

/**
 * 关掉欢迎信息时清掉列表里还没滚出去的欢迎行
 *
 * 欢迎行与弹幕混在同一份列表里，不像礼物区那样有个 `v-if` 能整块隐藏，
 * 不主动清的话最多会残留 120 条。
 */
function dropWelcomeRows() {
  danmakuList.value = danmakuList.value.filter(d => !d.isWelcome)
}

/**
 * 写入一条礼物行：同一连击分组的更新复用同一 id，按 id 覆盖（行位置不变），
 * 超出「礼物区最多显示条数」时丢弃最旧的。
 */
function upsertGift(b: BackingEvent) {
  const list = giftList.value
  const i = list.findIndex(x => x.id === b.id)
  if (i >= 0) {
    list[i] = b
  } else {
    list.push(b)
  }
  const max = giftMaxRows()
  if (list.length > max) {
    list.splice(0, list.length - max)
  }
}

/** 礼物区行数上限（至少 1，防止配置被改成 0 时列表全空） */
function giftMaxRows(): number {
  return Math.max(1, giftCfg.value.max_rows)
}

// 调小「最多显示条数」时立刻裁掉多出来的行，否则要等下一条礼物才生效
watch(giftMaxRows, max => {
  const list = giftList.value
  if (list.length > max) {
    list.splice(0, list.length - max)
  }
})

let unlistenDanmu: UnlistenFn | undefined
let unlistenRoom: UnlistenFn | undefined
let unlistenStyle: UnlistenFn | undefined
let unlistenFilter: UnlistenFn | undefined
let unlistenGift: UnlistenFn | undefined
let unlistenGiftCfg: UnlistenFn | undefined
let unlistenWelcome: UnlistenFn | undefined
let unlistenWelcomeCfg: UnlistenFn | undefined

/// 弹幕过滤判定：
/// 身份规则（抖音等级）命中即显示；未开启身份规则 = 不过滤；
/// 敏感词屏蔽独立叠加——开关开启且命中词表时整条丢弃（不豁免）。
function shouldShowDanmaku(d: DanmakuEvent, f: DanmakuFilter): boolean {
  if (f.enable_sensitive && f.sensitive_words.some(w => w && d.content.toLowerCase().includes(w.toLowerCase()))) {
    return false
  }
  if (!f.enable_level) return true
  // 服务端不一定每条弹幕都带等级：拿不到就当作不够级（与 Rust 侧 `matches_filter` 同语义）
  return (d.level ?? 0) >= f.level_min
}

onMounted(async () => {
  unlistenRoom = await listen<RoomStatusEvent>('room-status', e => {
    applyConnStatus(e.payload)
  })
  unlistenDanmu = await listen<DanmakuEvent>('danmaku', e => {
    const d = e.payload as DisplayDanmaku
    // 过滤：不满足配置规则（敏感词命中/等级不够）的弹幕整条丢弃
    if (!shouldShowDanmaku(d, filter.value)) {
      return
    }
    pushDanmaku(d)
  })
  unlistenGift = await listen<BackingEvent>('gift', e => {
    upsertGift(e.payload)
  })
  unlistenGiftCfg = await listen<GiftConfig>('gift-config', e => {
    giftCfg.value = e.payload
  })
  unlistenWelcome = await listen<WelcomeEvent>('welcome', e => {
    pushWelcome(e.payload)
  })
  // 关掉开关时把残留的欢迎行清掉（开的那一侧不用管：Rust 会开始广播）
  unlistenWelcomeCfg = await listen<WelcomeConfig>('welcome-config', e => {
    if (!e.payload.enabled) {
      dropWelcomeRows()
    }
  })
  unlistenStyle = await listen<OverlayStyle>('overlay-style', e => {
    style.value = e.payload
  })
  unlistenFilter = await listen<DanmakuFilter>('danmaku-filter', e => {
    filter.value = e.payload
  })
  // 初始同步样式
  try {
    style.value = await invoke<OverlayStyle>('overlay_get_style')
  } catch {
    /* ignore */
  }
  // 强制触发字体渲染重计算，避免首次加载时字体像素化
  await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))
  // 初始同步过滤配置
  try {
    filter.value = await invoke<DanmakuFilter>('danmaku_get_filter')
  } catch {
    /* ignore */
  }
  // 初始同步礼物列表配置（条数上限）
  try {
    giftCfg.value = await invoke<GiftConfig>('gift_get_config')
  } catch {
    /* ignore */
  }
  // 轮询兑底：事件丢失时也能同步连接状态（每 2s）
  const poll = async () => {
    try {
      const st = await invoke<RoomStatusEvent>('get_connection_status')
      applyConnStatus(st)
    } catch {
      /* overlay 独立打开等场景忽略 */
    }
  }
  await poll()
  const timer = setInterval(poll, 2000)
  onUnmounted(() => clearInterval(timer))
})

onUnmounted(() => {
  unlistenDanmu?.()
  unlistenRoom?.()
  unlistenStyle?.()
  unlistenFilter?.()
  unlistenGift?.()
  unlistenGiftCfg?.()
  unlistenWelcome?.()
  unlistenWelcomeCfg?.()
})
</script>

<template>
  <div
    class="overlay-root"
    :style="{
      fontSize: style.font_size + 'px',
      fontFamily: style.font_family,
      rowGap: style.row_gap + 'px',
    }"
    data-tauri-drag-region>
    <!-- 常驻透明拖拽条：不在弹幕流内，弹幕高速刷新时也能稳定拖动窗口；移入时显示操作提示 -->
    <div class="drag-strip" data-tauri-drag-region>
      <span class="drag-hint">按住此区域可拖动弹幕窗位置</span>
    </div>
    <!-- 面板：礼物区 + 弹幕区共用同一个圆角矩形与底色。
         圆角与背景放在外层，而不是给两个区各写一半：礼物区没内容时不渲染，
         把顶部圆角写在 .gift-area 上时顶部就会退化成直角。 -->
    <div class="panel" :style="{ backgroundColor: bgColor }">
      <!-- 礼物区：老板打赏，固定在弹幕区上方，与弹幕行共用徽章列（左边缘对齐） -->
      <div v-if="giftCfg.enabled && giftList.length > 0" class="gift-area">
        <GiftRow v-for="b in giftList" :key="b.id" :b="b" :overlay-style="style" />
      </div>
      <!-- 弹幕区：自带裁剪的独立容器，礼物区不会被高频弹幕顶出可视区 -->
      <div class="danmaku-area">
        <DanmakuRow v-for="d in danmakuList" :key="d.id" :d="d" :overlay-style="style" />
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 正文列起点：level-slot 5.6em + 0.7em 间距（与 MetaBadges.vue 的 .level-slot 必须一致）。
   5.6em 是量出来的：最长徽章组合（等级 + 灯牌）实测 5.28em。
   面板背景再往左让 0.35em（--panel-inset），正好落在徽章列与正文之间，
   两边各留 0.35em，徽章不会贴着背景边。
   --panel-inset 同时是发送弹幕框的左缩进（见 window.rs 的 sender_layout_metrics）。 */
.overlay-root {
  --level-indent: 6.3em;
  --panel-inset: calc(var(--level-indent) - 0.35em);
  position: relative;
  height: 100%;
  display: flex;
  flex-direction: column;
  justify-content: flex-end;
  padding: 6px;
  box-sizing: border-box;
  overflow: hidden;
  background: transparent;
  user-select: none;
}

/* 面板：礼物区 + 弹幕区共用的圆角矩形（底色由「背景不透明度」控制）。
   gap: inherit 让内部两区继续用弹幕窗根节点下发的 rowGap。

   背景左边缘要让出等级列（LV 徽章），但行内容必须留在原位，
   所以面板自身 margin-left 右移、内部两区再用等量负 margin 拉回来：
   只有背景走了，文字坐标不变。缩进取 --panel-inset（比 --level-indent 少 0.35em），
   背景与第一个徽章之间才有一段空隙。
   overflow 必须是 visible：徽章被拉到面板左侧之外后，若在这里裁剪就没了。 */
.panel {
  flex: 1 1 auto;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: inherit;
  margin-left: var(--panel-inset);
  border-radius: 15px;
  overflow: visible;
}

/* 礼物区：行间距跟随弹幕窗的 rowGap。
   flex-shrink: 0 = 不允许被弹幕挤掉：礼物是「值得看一眼」的信息，得一直在。 */
.gift-area {
  display: flex;
  flex-direction: column;
  gap: inherit;
  flex-shrink: 0;
  padding: 3px 0;
  margin-bottom: 3px;
  /* 拉回面板右移的那段，让行内容回到窗口原位（与面板 margin-left 等量反向） */
  margin-left: calc(-1 * var(--panel-inset));
}

/* 弹幕区：自带裁剪的滚动容器。
   min-height: 0 是关键——flex 子项默认 min-height: auto，会被内容撑到全高，
   结果连同上方礼物区一起溢出容器顶部被裁掉（就是「礼物看不见」的原因）。
   justify-content: flex-end 让新弹幕贴底、旧的从顶部裁掉，与改动前观感一致；
   padding-bottom 是给最新一条留的呼吸，不然文字直接顶着面板下边缘。 */
.danmaku-area {
  flex: 1 1 auto;
  min-height: 0;
  display: flex;
  flex-direction: column;
  justify-content: flex-end;
  gap: inherit;
  overflow: hidden;
  padding-bottom: 0.4em;
  /* 同 .gift-area：行回到窗口原位，只有背景面板右移 */
  margin-left: calc(-1 * var(--panel-inset));
}

/* 顶部常驻拖拽条：占位独立行（不压内容），平时透明；鼠标移入悬浮窗即浮出提示 */
.drag-strip {
  /* 不参与压缩，否则弹幕一多就被挤成一条线 */
  flex: 0 0 auto;
  /* 左端对齐面板背景左边缘（也避开了等级徽章） */
  margin-left: var(--panel-inset);
  height: 1.4em;
  cursor: move;
  background: transparent;
  border-bottom: 1px solid transparent;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  transition:
    background 0.12s ease,
    border-color 0.12s ease;
}

/* 操作提示：默认淡出，鼠标移入悬浮窗时显示；pointer-events 穿透保证整条可拖动 */
.drag-hint {
  font-size: 0.8em;
  line-height: 1;
  color: rgba(255, 255, 255, 0.92);
  opacity: 0;
  pointer-events: none;
  white-space: nowrap;
  transition: opacity 0.12s ease;
}

.overlay-root:hover .drag-strip {
  background: rgba(0, 0, 0, 0.55);
  border-bottom-color: rgba(255, 255, 255, 0.35);
}

.overlay-root:hover .drag-hint {
  opacity: 1;
}
</style>
