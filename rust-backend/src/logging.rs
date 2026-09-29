//! 轻量日志系统 —— 内存环形缓冲 + 会话日志 + 工程日志双通道落盘。
//!
//! 设计要点：
//!   * session 通道：`<cache_dir>/esrxp-session.log`，后端启动即绑定，尚未指定工程时同样有迹可循
//!   * project 通道：`<工程目录>/esrxp.log`，与导出产物同目录保存，跟随工程一起交付/复现问题
//!   * 每行写盘后立即 flush：进程异常退出（崩溃、强杀、Electron 重启）不丢最后一条线索
//!   * 单文件超过 MAX_BYTES 触发轮转：`esrxp.log → esrxp.log.1 → .2 → .3`，最多 3 个备份
//!   * 时间戳取系统本地时区（Windows 走 localtime_s，POSIX 走 localtime_r）

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 内存环形缓冲保留条数（超出后丢弃最旧的，UI 读不到文件也能看到近期记录）
const MAX_MEM: usize = 3000;
/// 单个日志文件轮转阈值
const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// 轮转备份个数
const KEEP_BACKUPS: usize = 3;

/// 工程日志文件名（位于用户指定的输出/工程目录）
pub const PROJECT_LOG_NAME: &str = "esrxp.log";
/// 会话日志文件名（位于缓存目录，跨工程留痕）
const SESSION_LOG_NAME: &str = "esrxp-session.log";

/// 一条日志记录
#[derive(Clone, Debug)]
pub struct Entry {
    pub ts: String,
    pub level: String,
    pub msg: String,
}

impl Entry {
    fn line(&self) -> String {
        format!("[{}] [{}] {}\n", self.ts, self.level, self.msg)
    }
}

/// 单条输出通道：文件句柄 + 路径 + 已写字节数（用于轮转）
struct Chan {
    file: Option<File>,
    path: Option<PathBuf>,
    bytes: u64,
}

impl Chan {
    fn empty() -> Self {
        Chan { file: None, path: None, bytes: 0 }
    }
}

struct Sink {
    mem: VecDeque<Entry>,
    session: Chan,
    project: Chan,
    dropped: u64,
    bound: Option<PathBuf>,
}

fn sink() -> &'static Mutex<Sink> {
    static S: OnceLock<Mutex<Sink>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Sink {
            mem: VecDeque::with_capacity(256),
            session: Chan::empty(),
            project: Chan::empty(),
            dropped: 0,
            bound: None,
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, Sink> {
    match sink().lock() {
        Ok(g) => g,
        // 上一位持有者在写盘时 panic：不清空状态，继续在已有内容上追加
        Err(p) => p.into_inner(),
    }
}

// ------------------------------------------------------------------ 本地时间戳
#[cfg(windows)]
unsafe fn fill_local(ts: libc::time_t, out: &mut libc::tm) {
    let _ = libc::localtime_s(out, &ts);
}

#[cfg(not(windows))]
unsafe fn fill_local(ts: libc::time_t, out: &mut libc::tm) {
    let _ = libc::localtime_r(&ts, out);
}

/// 当前本地时间 `YYYY-MM-DD HH:MM:SS.mmm`
fn stamp() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let ms = d.subsec_millis();
    let secs = d.as_secs() as libc::time_t;
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        fill_local(secs, &mut tm);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            ms
        )
    }
}

// ------------------------------------------------------------------ 文件轮转
fn backup_path(p: &Path, i: usize) -> PathBuf {
    PathBuf::from(format!("{}.{i}", p.display()))
}

/// 轮转当前日志：删除最老备份，`.2 → .3`、`.1 → .2`、`当前 → .1`
fn rotate(p: &Path) {
    let oldest = backup_path(p, KEEP_BACKUPS);
    if oldest.exists() {
        let _ = std::fs::remove_file(&oldest);
    }
    let mut i = KEEP_BACKUPS - 1;
    loop {
        let src = if i == 0 { p.to_path_buf() } else { backup_path(p, i) };
        let dst = backup_path(p, i + 1);
        if src.exists() {
            let _ = std::fs::rename(&src, &dst);
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
}

fn open_chan(dir: &Path, name: &str) -> Chan {
    let p = dir.join(name);
    if let Ok(md) = std::fs::metadata(&p) {
        if md.len() > MAX_BYTES {
            rotate(&p);
        }
    }
    let file = OpenOptions::new().create(true).append(true).open(&p).ok();
    let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    if file.is_some() {
        Chan { file, path: Some(p), bytes }
    } else {
        Chan::empty()
    }
}

/// 追加一行到通道，必要时轮转后重开
fn write_chan(ch: &mut Chan, line: &str) {
    let Some(f) = ch.file.as_mut() else { return };
    match f.write_all(line.as_bytes()).and_then(|_| f.flush()) {
        Ok(_) => ch.bytes += line.len() as u64,
        Err(_) => return,
    }
    if ch.bytes > MAX_BYTES {
        let prev = ch.path.clone();
        ch.file = None;
        ch.bytes = 0;
        if let Some(p) = prev {
            rotate(&p);
            let nc = open_chan(p.parent().unwrap_or_else(|| Path::new(".")),
                               p.file_name().and_then(|s| s.to_str()).unwrap_or("esrxp.log"));
            ch.file = nc.file;
            ch.bytes = nc.bytes;
            ch.path = nc.path.or(Some(p));
        }
    }
}

// ------------------------------------------------------------------ 对外接口
/// 绑定会话日志目录（= 后端 cache_dir），进程生命周期内只调一次
pub fn init_session(dir: &Path) {
    let ch = open_chan(dir, SESSION_LOG_NAME);
    let mut g = lock();
    g.session = ch;
}

/// 绑定工程日志目录：后续所有记录同步写入 `<dir>/esrxp.log`
pub fn bind_project(dir: &Path) -> Option<String> {
    let mut g = lock();
    if let Some(cur) = g.bound.as_ref() {
        if cur == dir {
            return g.project.path.as_ref().map(|p| p.display().to_string());
        }
    }
    let ch = open_chan(dir, PROJECT_LOG_NAME);
    let disp = ch.path.as_ref().map(|p| p.display().to_string());
    g.project = ch;
    g.bound = Some(dir.to_path_buf());
    disp
}

/// 当前绑定的工程日志路径
pub fn project_path() -> Option<String> {
    let g = lock();
    g.project.path.as_ref().map(|p| p.display().to_string())
}

/// 会话日志路径（无工程时的兜底落盘位置）
pub fn session_path() -> Option<String> {
    let g = lock();
    g.session.path.as_ref().map(|p| p.display().to_string())
}

/// 记录一条日志（level 建议 INFO / WARN / ERROR / DEBUG）
pub fn record(level: &str, msg: &str) {
    let e = Entry { ts: stamp(), level: level.to_string(), msg: msg.to_string() };
    let line = e.line();
    let mut g = lock();
    if g.mem.len() >= MAX_MEM {
        g.mem.pop_front();
        g.dropped += 1;
    }
    g.mem.push_back(e);
    write_chan(&mut g.session, &line);
    write_chan(&mut g.project, &line);
    // 严重级别同步打到 stderr，便于终端排障与崩溃转储对照
    if matches!(level, "ERROR" | "WARN") {
        eprint!("{line}");
    }
}

/// INFO 级（业务里程碑、任务起止、产物写出）
pub fn info<M: AsRef<str>>(m: M) {
    record("INFO", m.as_ref());
}

/// WARN 级（可恢复异常、降级处理）
pub fn warn<M: AsRef<str>>(m: M) {
    record("WARN", m.as_ref());
}

/// ERROR 级（任务失败、写出失败、非法请求）
pub fn error<M: AsRef<str>>(m: M) {
    record("ERROR", m.as_ref());
}

/// DEBUG 级（调试细节，默认同样落盘以便复现）
pub fn debug<M: AsRef<str>>(m: M) {
    record("DEBUG", m.as_ref());
}

/// 读取内存环形缓冲尾部 n 条（格式化为 `日志行` 文本）
pub fn tail_lines(n: usize) -> Vec<String> {
    let g = lock();
    let n = n.min(g.mem.len());
    g.mem.iter().skip(g.mem.len() - n).map(|e| e.line().trim_end().to_string()).collect()
}

/// 读取磁盘日志文件尾部（`max_lines` 行，按 UTF-8 有损解码）
pub fn read_tail(path: &Path, max_lines: usize) -> Result<Vec<String>, String> {
    let data = std::fs::read(path).map_err(|e| format!("日志读取失败: {e}"))?;
    let text = String::from_utf8_lossy(&data);
    let mut all: Vec<&str> = text.lines().collect();
    if all.len() > max_lines {
        all = all.split_off(all.len() - max_lines);
    }
    Ok(all.iter().map(|s| s.to_string()).collect())
}

/// (仅测试/诊断用) 已丢弃条数
pub fn dropped_count() -> u64 {
    lock().dropped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_has_local_date() {
        let s = stamp();
        assert_eq!(s.len(), 23, "stamp={s}");
        assert!(s.contains('-') && s.contains(':') && s.contains('.'));
    }

    #[test]
    fn memory_tail_keeps_order() {
        info("unit-a");
        warn("unit-b");
        let t = tail_lines(2);
        assert!(t.last().unwrap().ends_with("unit-b"), "{t:?}");
    }
}
