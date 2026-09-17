<script setup lang="ts">
import type { OverlayStyle } from '../types/ipc'

// 弹幕行 / 礼物行共用的身份徽章区：用户等级（荣誉等级）+ 粉丝团（灯牌）等级。
//
// 单独成组件是为了让两区共用同一套列宽（level-slot 4.8em）——复制一份 CSS 后
// 只改单侧，两区的正文列就不在同一竖线上（level-slot 也决定 OverlayApp 的 --level-indent
// 与发送框左缩进 window.rs）。
//
// 列宽固定而不是让徽章自己撑开：两个都没的弹幕也要占同样的位置，
// 否则用户名会在「有徽章 / 没徽章」两种行之间左右跳。
//
// 灯牌只有等级没有名字（`FansClubData.clubName` 实测为空，网页上那枚灯牌就是一张
// 带等级数字的图标），所以徽章文字写成「灯牌N」。灯牌由调用方传入：
// 弹幕行传（等级来自 `User.f24`）、礼物行不传（打赏行已有用户名与礼物名，再加一枚徽章只挤横向空间）。
defineProps<{
  /** 抖音用户等级（荣誉等级）；缺省 = 不渲染这一枚（列照占） */
  level?: number
  /** 粉丝团（灯牌）等级；缺省 = 未加入粉丝团，不渲染 */
  fansClubLevel?: number
  overlayStyle: OverlayStyle
}>()
</script>

<template>
  <span class="meta">
    <!-- 等级列（固定宽度，无徽章留空）→ 各行正文列垂直对齐 -->
    <span class="level-slot">
      <span v-if="overlayStyle.show_level && level && level > 0" class="chip lv">LV{{ level }}</span>
      <span v-if="overlayStyle.show_fansclub && fansClubLevel && fansClubLevel > 0" class="chip fansclub">
        灯牌{{ fansClubLevel }}
      </span>
    </span>
  </span>
</template>

<style scoped>
/* 徽章区：横向单行，不折行不压缩 */
.meta {
  display: flex;
  align-items: center;
  flex-shrink: 0;
  white-space: nowrap;
  /* 与正文首行文字视觉中轴对齐。用 em 不用 px：
     徽章盒高是 0.74em x 1.7，正文行高 1.65em，两者都随字号变，
     固定 px 时字号一大就往下错开（字号 20 时 3px 已偏差 3px）。 */
  margin-top: 0.3em;
}

/* 等级列：宽度 5.6em + 右间距 0.7em = 正文列起点。
   宽度是**量出来的**（headless Chrome 跑同一套 CSS，根字号 17px）：
   「LV21 + 灯牌10」这种最长组合 = 5.28em，故取 5.6em 留一点余量。
   为什么必须给够：这里是 justify-content: flex-end + overflow: hidden，
   内容一超宽就从**左边**裁——真机出过「LV20 显示成 V20」的事故。
   间距不能再小：面板背景左边缘要落在这一段的中间，否则徽章会贴着背景边（见 OverlayApp 的 --panel-inset）。
   改这里的宽度必须同步 OverlayApp 的 --level-indent 与 window.rs 的 sender_layout_metrics。 */
.level-slot {
  display: inline-flex;
  align-items: center;
  justify-content: flex-end;
  gap: 0.2em;
  width: 5.6em;
  margin-right: 0.7em;
  overflow: hidden;
}

/* 徽章：inline-block 内联排版，数字等宽，最小宽度保证短内容对齐 */
.chip {
  display: inline-block;
  vertical-align: middle;
  font-size: 0.74em;
  line-height: 1.7;
  font-weight: 700;
  padding: 0 0.3em;
  border-radius: 0.28em;
  text-shadow: none;
  text-align: center;
  white-space: nowrap;
  font-variant-numeric: tabular-nums;
  transform: translateY(-1px);
  flex-shrink: 0;
}

/* 等级徽章：金色。不设 min-width：徽章靠右站，LV8 与 LV21 差的那点宽度根本看不出来，
   而为了对齐去加 min-width 会把整列撑到 7.3em、白白把正文往右推 */
.chip.lv {
  color: #6b4a00;
  background: linear-gradient(180deg, #ffe08a, #f0b429);
}

/* 粉丝团（灯牌）徽章：深蓝底 + 白字（只写等级不写名字）。
   之前的浅蓝底（#7dd3fc）+ 白字对比度只有 ~1.9:1，真机反馈「看着费劲」；
   现在渐变两端与白字的对比度是 4.7:1 → 7.6:1（WCAG AA 要求 ≥ 4.5:1）。 */
.chip.fansclub {
  color: #fff;
  background: linear-gradient(180deg, #0d7ab8, #075985);
}
</style>
