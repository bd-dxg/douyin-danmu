//! 登录 helper：同一个 exe 的第二种身份（`douyin-danmu.exe --login-helper`）
//!
//! 主程序（`commands::douyin_login_open`）spawn 这个进程并等它退出；它加载抖音登录页、
//! 等扫码完成、把 Cookie 写到 **stdout 一行**，然后退出。
//!
//! 为什么单独进程：抖音首页会让 WebView2 自动播放视频流，GPU 进程峰值到 1 GB 上下，
//! 窗口关了还有约 84 MB 的 GPU 缓存残留（进程级缓存，不随窗口回收）。进程退出是唯一
//! 能确定全额归还内存的方式。
//!
//! 为什么走 stdout 而不是写 config.json：主程序每 2 秒在 flush overlay 位置落盘
//! （`window::flush_overlay_bounds`），两个进程同时改写同一个 config.json 会互相覆盖；
//! 而且管道不落盘，明文 Cookie 不会留在磁盘上（写盘仍旧只走 `config::save_auth`）。
//!
//! 退出码：0 = 成功（stdout 有 `LOGIN_COOKIE=<cookie>` 一行）；1 = 失败（原因在 stderr）。

use super::login::{create_login_window, wait_for_login, LOGIN_TIMEOUT};
use std::io::Write;

/// 触发 helper 身份的启动参数（主程序 spawn 自己时带上）
pub const LOGIN_HELPER_ARG: &str = "--login-helper";

/// 成功时 stdout 的行前缀（父进程照这个前缀找 Cookie）
pub const COOKIE_PREFIX: &str = "LOGIN_COOKIE=";

/// helper 进程主体：只做登录这一件事，拿到 Cookie 就退出
pub fn run_login_helper() {
    let app = tauri::Builder::default()
        // helper 写自己的日志文件：两个进程往同一个文件写会互相截断
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("douyin-danmu-login".into()),
                    },
                )])
                .level(log::LevelFilter::Info)
                .timezone_strategy(tauri_plugin_log::TimezoneStrategy::UseLocal)
                .build(),
        )
        .setup(|app| {
            let handle = app.handle().clone();
            let win = match create_login_window(&handle) {
                Ok(w) => w,
                Err(e) => fail(&e),
            };
            log::info!(
                "[login] 已打开抖音登录页，等待扫码（最多 {}s）",
                LOGIN_TIMEOUT.as_secs()
            );
            // 轮询必须异步：setup 在主线程上，阻塞在这里窗口永远显示不出来
            tauri::async_runtime::spawn(async move {
                match wait_for_login(&handle, &win).await {
                    Ok(cookie) => {
                        // 先销毁窗口再退：让 WebView2 控件走正常析构、把 user data folder
                        // 的句柄交还（进程硬终止虽然也会由 OS 释放，但留个干净的目录更省心）
                        let _ = win.destroy();
                        // 这一行是给父进程的 IPC 载荷，不是日志（日志一律走 log::*）
                        let mut out = std::io::stdout();
                        let _ = writeln!(out, "{COOKIE_PREFIX}{cookie}");
                        let _ = out.flush();
                        log::info!("[login] 扫码成功，Cookie 已交回主进程");
                        std::process::exit(0);
                    }
                    Err(e) => fail(&e),
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("登录进程启动失败");

    // 窗口被用户关掉时 run 会自己返回，此时没有任何结果可交，退出码交给下面的兜底
    app.run(|_, _| {});
    // 走到这里说明窗口关了、但没交回 Cookie：父进程按「未回传」处理
    fail("登录窗口已关闭，未完成登录");
}

/// 把失败原因写到 stderr（父进程捕获后记进自己的日志）并以退出码 1 结束
fn fail(reason: &str) -> ! {
    // 同 stdout：这是 IPC 载荷不是日志，不用 eprintln!（release 下 stderr 是管道，能通）
    let mut err = std::io::stderr();
    let _ = writeln!(err, "{reason}");
    let _ = err.flush();
    std::process::exit(1);
}
