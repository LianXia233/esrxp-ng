// preload：通过 contextBridge 暴露对话框 / 后端 API / 产物保存（渲染进程无 Node 权限）
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('esrxp', {
  selectVideo: () => ipcRenderer.invoke('select-video'),
  selectOutDir: () => ipcRenderer.invoke('select-outdir'),
  // 请求后端：主进程经命名管道转发，返回 { status, ct, b64 }
  request: (path, body) => ipcRenderer.invoke('api', path, body),
  // 保存产物到本地（弹出另存为对话框），返回保存路径或 null
  saveArtifact: (artifactPath, name) => ipcRenderer.invoke('save-artifact', artifactPath, name),
});
