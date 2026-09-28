// preload：通过 contextBridge 暴露文件对话框（渲染进程无 Node 权限）
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('esrxp', {
  selectVideo: () => ipcRenderer.invoke('select-video'),
  selectOutDir: () => ipcRenderer.invoke('select-outdir'),
});
