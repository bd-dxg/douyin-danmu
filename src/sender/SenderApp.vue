<script setup lang="ts">
import { invoke } from '@tauri-apps/api/core'
import { onMounted, onUnmounted, ref } from 'vue'

// 底部吸附小窗：**穿透开关**，不是发送框。
//
// 它的存在理由：穿透开启后弹幕窗不再接收鼠标事件（`set_ignore_cursor_events`），
// 放在弹幕窗里的按钮会点不到，必须有个不随穿透失效的独立窗口来关它。
// 发送弹幕仍未实现（要另一套签名 `a_bogus` + 登录 cookie），所以这里不放输入框。
const clickthrough = ref(false)

async function toggleClickthrough() {
  try {
    clickthrough.value = await invoke<boolean>('overlay_set_clickthrough', {
      enabled: !clickthrough.value,
    })
  } catch {
    /* 窗口未就绪等罕见失败：保持原状态即可 */
  }
}

async function syncState() {
  try {
    clickthrough.value = await invoke<boolean>('overlay_get_clickthrough')
  } catch {
    /* ignore */
  }
}

let pollTimer: ReturnType<typeof setInterval> | undefined

onMounted(async () => {
  await syncState()
  // 主界面「弹幕窗」里也能改穿透，轮询兑底保证按钮状态不失真
  pollTimer = setInterval(syncState, 2000)
})

onUnmounted(() => {
  if (pollTimer) clearInterval(pollTimer)
})
</script>

<template>
  <button
    type="button"
    class="ct-btn"
    :class="{ on: clickthrough }"
    :title="clickthrough ? '鼠标穿透：开（点击关闭）' : '鼠标穿透：关（点击开启）'"
    @click="toggleClickthrough"
    @contextmenu.prevent>
    {{ clickthrough ? '关闭穿透' : '开启穿透' }}
  </button>
</template>

<style scoped>
/* 按钮填满窗口（窗口尺寸由 Rust window.rs 的 CONTROL_W / CONTROL_H 定） */
.ct-btn {
  width: 100%;
  height: 100%;
  appearance: none;
  -webkit-appearance: none;
  cursor: pointer;
  white-space: nowrap;
  font-size: 13px;
  font-family: inherit;
  color: rgba(255, 255, 255, 0.88);
  background: rgba(0, 0, 0, 0.55);
  border: 1px solid rgba(255, 255, 255, 0.35);
  border-radius: 6px;
  transition:
    background 0.12s ease,
    color 0.12s ease,
    border-color 0.12s ease;
}

.ct-btn:hover {
  border-color: rgba(255, 255, 255, 0.6);
}

.ct-btn.on {
  color: #fff;
  background: rgba(120, 180, 255, 0.6);
  border-color: rgba(120, 180, 255, 0.95);
}
</style>
