// 无头冒烟验证：启动后端 → 加载 UI → 截图 → 模拟打开视频（注入路径）→ 截图
const { app, BrowserWindow } = require('electron');
const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

const PORT = 18082;
const SERVER = '/home/user/Doubao/chats/38444626264289538/esrxp-ng/rust-backend/target/debug/esrxp-ng-server';
const UI = '/home/user/Doubao/chats/38444626264289538/esrxp-ng/ui';
const VIDEO = '/home/user/Doubao/chats/38444626264289538/esrxp-ng/sample_hardsub.mp4';

let backend;
let win;

function waitFor() {
  return new Promise(res => {
    let n = 60;
    const t = () => {
      const http = require('http');
      const r = http.get(`http://127.0.0.1:${PORT}/api/info`, x => { x.resume(); res(true); });
      r.on('error', () => (n-- > 0 ? setTimeout(t, 250) : res(false)));
    };
    t();
  });
}

app.commandLine.appendSwitch('no-sandbox');
app.disableHardwareAcceleration();

app.whenReady().then(async () => {
  backend = spawn(SERVER, ['serve', '--host', '127.0.0.1', '--port', String(PORT), '--ui', UI, '--cache', '/tmp/esrxp_cache2'], { stdio: 'ignore' });
  const ok = await waitFor();
  if (!ok) { console.log('RESULT: backend-fail'); app.exit(1); return; }
  win = new BrowserWindow({ width: 1280, height: 900, show: false, backgroundColor: '#f3f5f8',
    webPreferences: { preload: path.join(__dirname, 'preload.js'), contextIsolation: true, nodeIntegration: false } });
  await win.loadURL(`http://127.0.0.1:${PORT}`);
  await new Promise(r => setTimeout(r, 4000));
  const img1 = await win.webContents.capturePage();
  fs.writeFileSync('/tmp/esrxp_ui_home.png', img1.toPNG());
  console.log('RESULT: home-screenshot-ok', img1.getSize());

  // 注入视频路径并点击打开：通过 executeJavaScript 调用 Vue 实例逻辑
  await win.webContents.executeJavaScript(`
    (async () => {
      const vm = window.__vm;
      vm.videoPath = ${JSON.stringify(VIDEO)};
      await vm.openVideo();
      await new Promise(r => setTimeout(r, 1500));
      // 预览第 30 帧
      vm.previewFrame = 30;
      await vm.doPreview();
      await new Promise(r => setTimeout(r, 2500));
      // 触发抓取（会跑 20 多秒，仅启动任务看进度 UI）
      return { w: vm.vinfo && vm.vinfo.width, h: vm.vinfo && vm.vinfo.height,
               previewSet: !!vm.previewUrl, cfgOk: vm.cfg.rip.frame_skip === 1 };
    })()
  `).then(async r => {
    console.log('RESULT: inject', JSON.stringify(r));
    await new Promise(res => setTimeout(res, 2000));
    const img2 = await win.webContents.capturePage();
    fs.writeFileSync('/tmp/esrxp_ui_video.png', img2.toPNG());
    console.log('RESULT: video-screenshot-ok');
    // 等待抓取完成（后端 20s+）
    await new Promise(res => setTimeout(res, 26000));
    const img3 = await win.webContents.capturePage();
    fs.writeFileSync('/tmp/esrxp_ui_done.png', img3.toPNG());
    console.log('RESULT: done-screenshot-ok');
    app.exit(0);
  }).catch(e => { console.log('RESULT: inject-fail', e.message); app.exit(1); });
});

app.on('quit', () => { if (backend) backend.kill(); });
