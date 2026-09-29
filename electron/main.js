// esrxp-ng Electron 外壳（Win11 目标）
// 职责：启动内置 Rust 后端（esrxp-ng-server），经命名管道/Unix socket 通信（不开任何网络端口），
//       加载本地 UI 文件，桥接文件对话框、API 请求与产物保存。
const { app, BrowserWindow, dialog, ipcMain, shell } = require('electron');
const http = require('http');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawn } = require('child_process');

// 0.4.0 起不再监听 TCP 端口：打包态经管道通信，规避端口占用/系统代理劫持/跨进程失联
const PIPE_NAME = process.platform === 'win32'
  ? '\\\\.\\pipe\\esrxp-ng-backend'
  : path.join(os.tmpdir(), 'esrxp-ng-backend.sock');

// 单实例锁：0.3.2 实测双实例导致第二实例后端绑端口失败、错挂到第一实例后端，
// 后端一崩 UI 全线 Failed to fetch。单实例 + second-instance 聚焦既有窗口。
if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {

app.on('second-instance', () => {
  if (mainWin && !mainWin.isDestroyed()) {
    if (mainWin.isMinimized()) mainWin.restore();
    mainWin.show();
    mainWin.focus();
  }
});

// 启动日志：写 userData/startup.log，白屏/启动失败时用户可据此反馈
function log(...args) {
  const msg = args.map(a => typeof a === 'string' ? a : JSON.stringify(a)).join(' ');
  console.log('[esrxp-ng]', msg);
  try {
    const dir = app.getPath('userData');
    fs.mkdirSync(dir, { recursive: true });
    fs.appendFileSync(path.join(dir, 'startup.log'), new Date().toISOString() + ' ' + msg + '\n');
  } catch (_) {}
}

// 后端二进制：优先环境变量 ESRXP_SERVER，其次 extraResources 真实路径（resources/server），
// 再其次 app.asar.unpacked/server，最后回退开发态构建。
function resolveServer() {
  const exeName = process.platform === 'win32' ? 'esrxp-ng-server.exe' : 'esrxp-ng-server';
  const candidates = [
    process.env.ESRXP_SERVER,
    path.join(process.resourcesPath, 'server', exeName),                       // extraResources 输出
    path.join(process.resourcesPath, 'app.asar.unpacked', 'server', exeName),  // asarUnpack 解包
    path.join(__dirname, 'server', exeName),                                   // 开发态 electron/server
    path.join(__dirname, '..', 'rust-backend', 'target', 'release', exeName),
    path.join(__dirname, '..', 'rust-backend', 'target', 'debug', exeName),
  ].filter(Boolean);
  for (const c of candidates) {
    try { if (c && fs.existsSync(c)) { log('后端二进制: ' + c); return c; } } catch (_) {}
  }
  return null;
}

function resolveUiDir() {
  if (app.isPackaged) {
    const extra = path.join(process.resourcesPath, 'ui');                          // extraResources 输出（0.3.2 起）
    if (fs.existsSync(extra)) { log('UI 目录: ' + extra); return extra; }
    const unpacked = path.join(process.resourcesPath, 'app.asar.unpacked', 'ui');  // asarUnpack 兜底
    if (fs.existsSync(unpacked)) { log('UI 目录: ' + unpacked); return unpacked; }
    log('警告: 打包态未找到 UI 资源目录（检查 electron-builder extraResources 配置）');
    return extra;
  }
  const dev = path.join(__dirname, '..', 'ui');
  log('UI 目录(开发态): ' + dev);
  return dev;
}

// 管道 HTTP 请求：Node http over socketPath（Windows 命名管道 / Unix domain socket 均支持），
// 与后端 axum 之间仍是标准 HTTP/1.1，协议语义与旧 TCP 模式完全一致。
function pipeRequest(reqPath, method, bodyObj) {
  return new Promise((resolve, reject) => {
    const data = bodyObj !== undefined ? JSON.stringify(bodyObj) : null;
    const req = http.request({
      socketPath: PIPE_NAME,
      path: reqPath,
      method: method || 'GET',
      headers: data ? { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(data) } : {},
      timeout: 300000,
    }, res => {
      const chunks = [];
      res.on('data', c => chunks.push(c));
      res.on('end', () => resolve({ status: res.statusCode, ct: res.headers['content-type'] || '', buf: Buffer.concat(chunks) }));
    });
    req.on('error', reject);
    req.on('timeout', () => { req.destroy(new Error('pipe request timeout')); });
    if (data) req.write(data);
    req.end();
  });
}

let backend = null;
let mainWin = null;
let backendArmed = false; // 启动阶段完成后才启用「后端退出即告警」，避免启动期误报
let quitting = false;

// ---- 后端启动（管道模式，无端口）----
function startBackend() {
  const bin = resolveServer();
  if (!bin) {
    log('未找到 esrxp-ng-server 可执行文件（检查 ESRXP_SERVER / resources/server / electron/server）');
    return { ok: false, reason: 'server-missing' };
  }
  const uiDir = resolveUiDir();
  const cacheDir = path.join(app.getPath('userData'), 'cache');
  fs.mkdirSync(cacheDir, { recursive: true });
  backend = spawn(bin, ['serve', '--ui', uiDir, '--cache', cacheDir, '--pipe', PIPE_NAME], {
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  });
  let stderrBuf = '';
  backend.stdout.on('data', d => log('[backend]', d.toString().trim()));
  backend.stderr.on('data', d => { stderrBuf = (stderrBuf + d.toString()).slice(-4000); log('[backend]', d.toString().trim()); });
  backend.on('exit', code => {
    log('[backend] exited', code, stderrBuf ? 'stderr尾: ' + stderrBuf.slice(-400) : '');
    // 运行期后端崩溃：明确报错而非 UI 静默 Failed to fetch（0.3.2 实测教训）
    if (backendArmed && !quitting) {
      try {
        dialog.showErrorBox('esrxp-ng 后端已退出',
          '后端进程异常终止（退出码 ' + code + '）。\n\n' +
          (stderrBuf ? '后端输出（尾部）：\n' + stderrBuf.slice(-800) + '\n\n' : '') +
          '请查看 用户数据目录/startup.log 并反馈开发者。');
      } catch (_) {}
      if (mainWin && !mainWin.isDestroyed()) mainWin.destroy();
      app.exit(1);
    }
  });
  backend.stderrBuf = () => stderrBuf;
  return { ok: true };
}

// 探活：/api/info 返回 JSON 且含 version 字段
async function backendAlive() {
  try {
    const r = await pipeRequest('/api/info', 'GET');
    if (r.status !== 200) return false;
    const j = JSON.parse(r.buf.toString('utf8'));
    return !!(j && j.version);
  } catch (_) { return false; }
}

async function waitForBackend() {
  for (let i = 0; i < 60; i++) {
    if (await backendAlive()) return true;
    await new Promise(r => setTimeout(r, 250));
  }
  return false;
}

function showFatal(title, body) {
  try { dialog.showErrorBox(title, body); } catch (_) {}
  if (mainWin && !mainWin.isDestroyed()) mainWin.destroy();
  app.exit(1);
}

// ---- 窗口与加载兜底（白屏防御保留）----
function createWindow() {
  const uiDir = resolveUiDir();
  mainWin = new BrowserWindow({
    width: 1280,
    height: 900,
    minWidth: 960,
    minHeight: 640,
    autoHideMenuBar: true,
    backgroundColor: '#f3f5f8',
    show: false, // 加载完成再显示，避免白屏闪烁
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  mainWin.once('ready-to-show', () => mainWin.show());

  // 渲染进程崩溃 / 页面加载失败：给出明确诊断而非白屏
  mainWin.webContents.on('render-process-gone', (e, details) => {
    log('渲染进程崩溃:', details.reason);
    showFatal('esrxp-ng 渲染进程异常', '渲染进程已退出（' + details.reason + '）。请尝试重启应用；若仍复现，将 userData/startup.log 反馈给开发者。');
  });
  mainWin.webContents.on('did-fail-load', (e, code, desc, url) => {
    log('页面加载失败:', code, desc, url);
    mainWin.webContents.loadURL('data:text/html;charset=utf-8,' + encodeURIComponent(
      '<div style="font-family:Microsoft YaHei,sans-serif;padding:40px;color:#333">' +
      '<h2>esrxp-ng 页面加载失败</h2><p>错误码 ' + code + '：' + desc + '</p>' +
      '<p style="color:#888;font-size:13px">请查看 用户数据目录/startup.log 并反馈开发者。</p></div>'));
  });

  mainWin.webContents.setWindowOpenHandler(({ url }) => {
    if (url.startsWith('http')) shell.openExternal(url);
    return { action: 'deny' };
  });
  mainWin.on('closed', () => { mainWin = null; });

  // UI 直接从磁盘加载，不再经 HTTP 托管
  mainWin.loadFile(path.join(uiDir, 'index.html'));
  log('加载 UI: ' + path.join(uiDir, 'index.html'));
}

// ---- IPC 桥 ----
ipcMain.handle('api', async (e, reqPath, body) => {
  const r = await pipeRequest(reqPath, body !== undefined ? 'POST' : 'GET', body);
  return { status: r.status, ct: r.ct, b64: r.buf.toString('base64') };
});

ipcMain.handle('save-artifact', async (e, artifactPath, name) => {
  const r = await pipeRequest('/api/artifact?path=' + encodeURIComponent(artifactPath), 'GET');
  if (r.status !== 200) throw new Error('读取产物失败: HTTP ' + r.status);
  const w = await dialog.showSaveDialog(mainWin, { defaultPath: name || path.basename(artifactPath) });
  if (w.canceled || !w.filePath) return null;
  fs.writeFileSync(w.filePath, r.buf);
  return w.filePath;
});

ipcMain.handle('select-video', async () => {
  const r = await dialog.showOpenDialog(mainWin, {
    title: '选择视频文件',
    properties: ['openFile'],
    filters: [
      { name: '视频', extensions: ['mp4', 'mkv', 'avi', 'mov', 'wmv', 'ts', 'm2ts', 'flv', 'webm', 'rmvb'] },
      { name: '所有文件', extensions: ['*'] },
    ],
  });
  return r.canceled ? null : r.filePaths[0];
});

ipcMain.handle('select-outdir', async () => {
  const r = await dialog.showOpenDialog(mainWin, {
    title: '选择输出目录',
    properties: ['openDirectory', 'createDirectory'],
  });
  return r.canceled ? null : r.filePaths[0];
});

app.whenReady().then(async () => {
  log('app ready, packaged=' + app.isPackaged + ', platform=' + process.platform + ', pipe=' + PIPE_NAME);
  const st = startBackend();
  if (!st.ok) {
    showFatal('后端缺失', '未找到 esrxp-ng-server 可执行文件。请重新安装客户端，或设置 ESRXP_SERVER 环境变量指向后端路径。');
    return;
  }
  const ok = await waitForBackend();
  if (!ok) {
    const tail = backend && backend.stderrBuf ? backend.stderrBuf().slice(-800) : '';
    log('后端连接失败，stderr:', tail || '(空)');
    showFatal('后端启动失败',
      '无法连接 esrxp-ng-server（管道 ' + PIPE_NAME + '）。\n\n后端输出：\n' + (tail || '(无输出，可能是 FFmpeg 运行库缺失)') +
      '\n\n请查看 用户数据目录/startup.log 并反馈开发者。');
    return;
  }
  createWindow();
  backendArmed = true; // 窗口就绪后启用后端退出告警
  app.on('activate', () => { if (BrowserWindow.getAllWindows().length === 0) createWindow(); });
});

app.on('before-quit', () => { quitting = true; });

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});

app.on('quit', () => {
  if (backend) { try { backend.kill(); } catch (_) {} }
});

} // end single-instance lock
