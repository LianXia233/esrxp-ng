// esrxp-ng Electron 外壳（Win11 目标）
// 职责：启动内置 Rust 后端（esrxp-ng-server serve），加载 TDesign UI，桥接本地文件对话框。
const { app, BrowserWindow, dialog, ipcMain, shell } = require('electron');
const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

// GPU 硬件加速保持启用（0.3.1 曾全局禁用，0.3.2 撤销）；个别环境若渲染异常，
// 用户可自行以 --disable-gpu 启动参数兜底，不再影响全部环境。

const PORT_BASE = 18080;
const HOST = '127.0.0.1';

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
    path.join(process.resourcesPath, 'server', exeName),                 // extraResources 输出
    path.join(process.resourcesPath, 'app.asar.unpacked', 'server', exeName), // asarUnpack 解包
    path.join(__dirname, 'server', exeName),                             // 开发态 electron/server
    path.join(__dirname, '..', 'rust-backend', 'target', 'release', exeName),
    path.join(__dirname, '..', 'rust-backend', 'target', 'debug', exeName),
  ].filter(Boolean);
  for (const c of candidates) {
    try { if (c && fs.existsSync(c)) { log('后端二进制: ' + c); return c; } } catch (_) {}
  }
  return null;
}

let backend = null;
let mainWin = null;
let chosenPort = PORT_BASE;

function resolveUiDir() {
  if (app.isPackaged) {
    const extra = path.join(process.resourcesPath, 'ui');                  // extraResources 输出（0.3.2 起）
    if (fs.existsSync(extra)) { log('UI 目录: ' + extra); return extra; }
    const unpacked = path.join(process.resourcesPath, 'app.asar.unpacked', 'ui'); // asarUnpack 兜底
    if (fs.existsSync(unpacked)) { log('UI 目录: ' + unpacked); return unpacked; }
    log('警告: 打包态未找到 UI 资源目录，页面将为 404 空白（检查 electron-builder extraResources 配置）');
    return extra;
  }
  const dev = path.join(__dirname, '..', 'ui');
  log('UI 目录(开发态): ' + dev);
  return dev;
}

// ---- 后端启动（含端口占用自动规避，白屏防御保留）----
function startBackend(candidates) {
  const bin = resolveServer();
  if (!bin) {
    log('未找到 esrxp-ng-server 可执行文件（检查 ESRXP_SERVER / resources/server / electron/server）');
    return { ok: false, reason: 'server-missing' };
  }
  const uiDir = resolveUiDir();
  const cacheDir = path.join(app.getPath('userData'), 'cache');
  fs.mkdirSync(cacheDir, { recursive: true });
  backend = spawn(bin, ['serve', '--host', HOST, '--port', String(chosenPort), '--ui', uiDir, '--cache', cacheDir], {
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  });
  let stderrBuf = '';
  backend.stdout.on('data', d => log('[backend]', d.toString().trim()));
  backend.stderr.on('data', d => { stderrBuf = (stderrBuf + d.toString()).slice(-2000); log('[backend]', d.toString().trim()); });
  backend.on('exit', code => log('[backend] exited', code));
  backend.stderrBuf = () => stderrBuf;
  return { ok: true };
}

// 校验「真」是 esrxp-ng 后端：/api/info 返回 JSON 且含 version 字段（避免端口被其他 HTTP 服务占用误判）
function checkBackend(port) {
  return new Promise(resolve => {
    const http = require('http');
    const req = http.get({ host: HOST, port, path: '/api/info', timeout: 2000 }, res => {
      let body = '';
      res.on('data', c => { body += c; });
      res.on('end', () => {
        try {
          const j = JSON.parse(body);
          resolve(!!(j && j.version));
        } catch (_) { resolve(false); }
      });
    });
    req.on('error', () => resolve(false));
    req.on('timeout', () => { req.destroy(); resolve(false); });
  });
}

async function waitForBackend() {
  for (let i = 0; i < 60; i++) {
    if (await checkBackend(chosenPort)) return true;
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

  mainWin.loadURL(`http://${HOST}:${chosenPort}`);
  log('加载 UI: http://' + HOST + ':' + chosenPort);
}

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
  log('app ready, packaged=' + app.isPackaged + ', platform=' + process.platform);
  const st = startBackend();
  if (!st.ok) {
    showFatal('后端缺失', '未找到 esrxp-ng-server 可执行文件。请重新安装客户端，或设置 ESRXP_SERVER 环境变量指向后端路径。');
    return;
  }
  const ok = await waitForBackend();
  if (!ok) {
    const tail = backend && backend.stderrBuf ? backend.stderrBuf().slice(-800) : '';
    log('后端连接失败，stderr:', tail || '(空)');
    // 端口可能被占用：换端口重试一次（规避 18080 被其他程序占用导致的白屏/失败）
    const next = chosenPort + 1;
    if (next <= 18085) {
      log('端口 ' + chosenPort + ' 不可用，改用 ' + next);
      try { backend.kill(); } catch (_) {}
      chosenPort = next;
      const st2 = startBackend();
      if (st2.ok && await waitForBackend()) { createWindow(); return; }
    }
    showFatal('后端启动失败',
      '无法连接 esrxp-ng-server。\n\n后端输出：\n' + (tail || '(无输出，可能是 FFmpeg 运行库缺失)') +
      '\n\n请查看 用户数据目录/startup.log 并反馈开发者。');
    return;
  }
  createWindow();
  app.on('activate', () => { if (BrowserWindow.getAllWindows().length === 0) createWindow(); });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});

app.on('quit', () => {
  if (backend) { try { backend.kill(); } catch (_) {} }
});
