// esrxp-ng Electron 外壳（Win11 目标）
// 职责：启动内置 Rust 后端（esrxp-ng-server serve），加载 TDesign UI，桥接本地文件对话框。
const { app, BrowserWindow, dialog, ipcMain, shell } = require('electron');
const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

const PORT = 18080;
const HOST = '127.0.0.1';

// 后端二进制：优先环境变量 ESRXP_SERVER，其次与 main.js 同级的 server 可执行文件，
// 最后回退到 rust-backend/target 下的调试构建（开发态）。
function resolveServer() {
  const candidates = [
    process.env.ESRXP_SERVER,
    path.join(__dirname, 'server', process.platform === 'win32' ? 'esrxp-ng-server.exe' : 'esrxp-ng-server'),
    path.join(__dirname, '..', 'rust-backend', 'target', 'release', process.platform === 'win32' ? 'esrxp-ng-server.exe' : 'esrxp-ng-server'),
    path.join(__dirname, '..', 'rust-backend', 'target', 'debug', process.platform === 'win32' ? 'esrxp-ng-server.exe' : 'esrxp-ng-server'),
  ].filter(Boolean);
  for (const c of candidates) {
    if (c && fs.existsSync(c)) return c;
  }
  return null;
}

let backend = null;
let mainWin = null;

function startBackend() {
  const bin = resolveServer();
  if (!bin) {
    console.error('未找到 esrxp-ng-server 可执行文件，请先构建 Rust 后端或设置 ESRXP_SERVER');
    return false;
  }
  const uiDir = path.join(__dirname, '..', 'ui');
  const cacheDir = path.join(app.getPath('userData'), 'cache');
  fs.mkdirSync(cacheDir, { recursive: true });
  backend = spawn(bin, ['serve', '--host', HOST, '--port', String(PORT), '--ui', uiDir, '--cache', cacheDir], {
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  });
  backend.stdout.on('data', d => console.log('[backend]', d.toString().trim()));
  backend.stderr.on('data', d => console.error('[backend]', d.toString().trim()));
  backend.on('exit', code => console.log('[backend] exited', code));
  return true;
}

function waitForBackend(retries = 60) {
  return new Promise(resolve => {
    const tryOnce = () => {
      const http = require('http');
      const req = http.get(`http://${HOST}:${PORT}/api/info`, res => {
        res.resume();
        resolve(true);
      });
      req.on('error', () => {
        if (retries-- > 0) setTimeout(tryOnce, 250);
        else resolve(false);
      });
    };
    tryOnce();
  });
}

function createWindow() {
  mainWin = new BrowserWindow({
    width: 1280,
    height: 900,
    minWidth: 960,
    minHeight: 640,
    autoHideMenuBar: true,
    backgroundColor: '#f3f5f8',
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  mainWin.loadURL(`http://${HOST}:${PORT}`);
  mainWin.webContents.setWindowOpenHandler(({ url }) => {
    if (url.startsWith('http')) shell.openExternal(url);
    return { action: 'deny' };
  });
  mainWin.on('closed', () => { mainWin = null; });
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
  startBackend();
  const ok = await waitForBackend();
  if (!ok) {
    dialog.showErrorBox('后端启动失败', '无法连接 esrxp-ng-server。请确认 Rust 后端可执行文件存在（rust-backend/target/release 或 electron/server/）。');
    app.quit();
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
