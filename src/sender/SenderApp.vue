<script setup lang="ts">
import { ref } from 'vue'

// 发送弹幕框：**当前版本只读**（Task/task_plan.md 已做的决策：首期不做发送弹幕）。
//
// 抖音发送要走另一套签名（`a_bogus`）+ 登录 cookie，比只读链路复杂一倍，
// 所以这里保持窗口与吸附逻辑不变，但把输入框钉在禁用态并把原因写在框里 ——
// 不假装能发，也不至于让主播对着一按没反应的输入框纳闷。
// 后续要做发送时，把 `send_danmaku` 命令接上真实实现、再恢复输入态即可。
const fontSize = ref(17)
</script>

<template>
  <div class="sender-root" :style="{ fontSize: fontSize + 'px' }">
    <input class="send-input" type="text" disabled placeholder="抖音发送弹幕暂未支持" />
    <div class="status-line">
      <span>当前版本只读：弹幕 / 礼物 / 进场 / 关注 / 点赞</span>
    </div>
  </div>
</template>

<style scoped>
.sender-root {
  display: flex;
  flex-direction: column;
  height: 100%;
  box-sizing: border-box;
  overflow: hidden;
  padding: 0.25em 0;
}

.send-input {
  width: 100%;
  appearance: none;
  -webkit-appearance: none;
  background: rgba(0, 0, 0, 0.55);
  border: 1px solid rgba(255, 255, 255, 0.35);
  border-radius: 0.4em;
  color: #fff;
  padding: 0.4em 0.6em;
  font-size: 1em;
  outline: none;
  user-select: text;
}

.send-input::placeholder {
  color: rgba(255, 255, 255, 0.5);
}

.send-input:disabled {
  cursor: not-allowed;
}

.status-line {
  padding: 0.15em 0.15em 0;
  font-size: 0.75em;
  flex-shrink: 0;
  min-height: 1.2em;
  color: rgba(255, 255, 255, 0.85);
  text-shadow:
    0 0 2px rgba(0, 0, 0, 0.9),
    0 0 2px rgba(0, 0, 0, 0.9);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
