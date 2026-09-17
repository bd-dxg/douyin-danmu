import { invoke } from '@tauri-apps/api/core'
// 登录状态（模块级单例，多组件共享）
import { ref } from 'vue'

import type { LoginInfo } from '../types/ipc'

const loggedIn = ref(false)

/**
 * 从 Rust 查询最新登录态。
 *
 * 抖音的登录只为拿到推礼物消息所需的 Cookie：弹幕、进场、关注、点赞匿名就能收，
 * 所以「未登录」不像 B 站那样等于不可用。
 */
export async function refreshLogin() {
  const info = await invoke<LoginInfo>('get_login_info')
  loggedIn.value = info.loggedIn
  return info
}

/**
 * 扫码登录：Rust 会开一个窗口加载抖音登录页，用户扫码成功后窗口自动关闭。
 * 命令一直悬着直到成功 / 超时 / 窗口被关，故调用方要自己显示进行中状态。
 */
export async function startLogin() {
  await invoke('douyin_login_open')
  await refreshLogin()
}

export function useLogin() {
  return {
    loggedIn,
    async logout() {
      await invoke('logout')
      loggedIn.value = false
    },
  }
}
