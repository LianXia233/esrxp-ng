//! 后端进程管理：发现并启动 esrxp-ng-server 子进程（随机空闲端口），
//! 提供就绪探测与退出清理。
//!
//! 跨平台：统一走 127.0.0.1 TCP（开发态与打包态一致）；不依赖命名管道/特定 Shell。

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Backend {
    child: Option<Child>,
    pub base_url: String,
}

impl Backend {
    /// 启动后端子进程，等待 /api/info 就绪后返回。
    pub fn start() -> Result<Self, String> {
        let exe = find_backend()?;
        // 预绑定空闲端口并让给后端监听（本地竞态窗口可忽略）
        let port = {
            let l = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
            l.local_addr().map_err(|e| e.to_string())?.port()
        };
        let base_url = format!("http://127.0.0.1:{port}");
        let cache_dir = std::env::temp_dir().join("esrxp-ng");
        std::fs::create_dir_all(&cache_dir).map_err(|e| e.to_string())?;
        let mut child = Command::new(&exe)
            .args([
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--cache",
                cache_dir.to_str().unwrap_or(""),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动后端失败（{}）：{e}", exe.display()))?;
        // 就绪探测：/api/info 返回 JSON 即认为可用
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(rep) = crate::client::get_json(&base_url, "/api/info") {
                if rep.get("name").is_some() {
                    break;
                }
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                return Err(format!("后端 {} 启动超时", exe.display()));
            }
            std::thread::sleep(Duration::from_millis(120));
        }
        // 吞噬 stdout/stderr：句柄不读会泄漏，且管道写满会阻塞子进程
        if let Some(mut pipe) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    if pipe.read(&mut buf).unwrap_or(0) == 0 {
                        break;
                    }
                }
            });
        }
        if let Some(mut pipe) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    if pipe.read(&mut buf).unwrap_or(0) == 0 {
                        break;
                    }
                }
            });
        }
        Ok(Self {
            child: Some(child),
            base_url,
        })
    }

    pub fn restart(&mut self) -> Result<(), String> {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let mut next = Self::start()?;
        self.child = next.child.take();
        self.base_url = next.base_url.clone();
        Ok(())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// 后端二进制查找顺序：
/// 1) ESRXP_BACKEND 环境变量；2) 与 GUI 同目录（打包态）；3) 开发态 rust-backend/target/debug；4) PATH。
fn find_backend() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("ESRXP_BACKEND") {
        if Path::new(&p).is_file() {
            return Ok(PathBuf::from(p));
        }
    }
    let exe_suffix = std::env::consts::EXE_SUFFIX; // "" 或 ".exe"
    let bin = format!("esrxp-ng-server{exe_suffix}");
    let cur = std::env::current_exe().map_err(|e| e.to_string())?;
    let cur_dir = cur.parent().ok_or("无法定位程序目录")?;
    let same = cur_dir.join(&bin);
    if same.is_file() {
        return Ok(same);
    }
    // 开发态：<workspace>/rust-backend/target/<profile>/esrxp-ng-server
    let dev = cur_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|root| {
            root.join("rust-backend")
                .join("target")
                .join("debug")
                .join(&bin)
        });
    if let Some(p) = dev {
        if p.is_file() {
            return Ok(p);
        }
    }
    Ok(PathBuf::from(&bin)) // PATH 兜底
}
