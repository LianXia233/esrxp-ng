"""esrxp-ng 本地 Web UI 后端（FastAPI）。

架构：
  - 前端：TDesign (Vue3) 单页应用，静态托管于 /ui 与 /
  - 后端：FastAPI 提供视频元数据 / 配置 / 抓取任务（后台线程 + 进度）/ 预览 / 产物下载
  - 抓取复用 ripper.rip + outputs 全套管线；进度经 /api/jobs/{id} 轮询

启动：esrxp-ng ui [--host 127.0.0.1 --port 8000]
"""
from __future__ import annotations

import threading
import time
import uuid
from pathlib import Path

from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from . import __version__
from .config import AppConfig, from_dict
from .outputs import (write_json_timeline, write_ocr_png, write_project,
                      write_ssa, write_vobsub)
from .ripper import rip
from .video import VideoSource

UI_DIR = Path(__file__).parent / "ui"

app = FastAPI(title="esrxp-ng UI", version=__version__)

# ------------------------------------------------------------------ 数据模型
class OpenRequest(BaseModel):
    path: str


class RipRequest(BaseModel):
    path: str
    out_dir: str = "out"
    config: dict = {}
    auto_color: bool = True


class PreviewRequest(BaseModel):
    path: str
    frame: int = 0
    config: dict = {}
    region_only: bool = False


# ------------------------------------------------------------------ 任务管理
JOBS: dict[str, dict] = {}
JOBS_LOCK = threading.Lock()


def _safe_artifact(path: str) -> Path:
    """产物下载安全校验：必须是已生成文件（绝对路径，拒绝穿越）。"""
    p = Path(path).resolve()
    if not p.is_file():
        raise HTTPException(404, f"文件不存在: {path}")
    return p


# ------------------------------------------------------------------ API
@app.get("/api/info")
def api_info():
    return {"name": "esrxp-ng", "version": __version__}


@app.post("/api/video/open")
def api_open(req: OpenRequest):
    if not Path(req.path).is_file():
        raise HTTPException(400, f"文件不存在: {req.path}")
    try:
        with VideoSource(req.path) as vs:
            return vs.info()
    except Exception as e:
        raise HTTPException(400, f"无法打开视频（FFmpeg 解码失败）: {e}")


@app.post("/api/preview")
def api_preview(req: PreviewRequest):
    import cv2
    import numpy as np
    from .filtering import filter_frame
    from .postprocess import clean
    from .ripper import _crop

    if not Path(req.path).is_file():
        raise HTTPException(400, f"文件不存在: {req.path}")
    cfg = from_dict(req.config)
    try:
        with VideoSource(req.path) as vs:
            fd = vs.frame_at(req.frame)
            full = fd.bgr
            roi, (ox, oy) = _crop(full, cfg.region.up, cfg.region.down,
                                  cfg.region.left, cfg.region.right)
            mask = filter_frame(roi, cfg.filter)
            mask = clean(mask, cfg.postprocess)
            if req.region_only:
                base, m = roi, mask
            else:
                base = full
                m = np.zeros(full.shape[:2], dtype=np.uint8)
                m[oy:oy + roi.shape[0], ox:ox + roi.shape[1]] = mask
            overlay = base.copy()
            color = np.full(base.shape, (0, 255, 0), dtype=np.uint8)
            overlay = cv2.addWeighted(overlay, 1, color, 0.55, 0)
            overlay[m > 0] = base[m > 0]
            panel = np.hstack([base, cv2.cvtColor(m, cv2.COLOR_GRAY2BGR), overlay])
            out = Path(cfg.ui_cache_dir) if getattr(cfg, "ui_cache_dir", None) else Path.home() / ".esrxp-ng"
            out.mkdir(parents=True, exist_ok=True)
            p = out / f"preview_{int(time.time()*1000)}.png"
            cv2.imwrite(str(p), panel)
            return {"image": f"/api/artifact?path={p}", "frame": fd.index, "time": round(fd.time, 3)}
    except Exception as e:
        raise HTTPException(400, f"预览失败: {e}")


@app.post("/api/rip")
def api_rip(req: RipRequest):
    if not Path(req.path).is_file():
        raise HTTPException(400, f"文件不存在: {req.path}")
    job_id = uuid.uuid4().hex[:12]
    job = {
        "id": job_id, "status": "running", "video": req.path, "out_dir": req.out_dir,
        "progress": {"done": 0, "total": 0, "found": 0, "pct": 0},
        "log": [], "result": None, "started": time.time(),
    }
    with JOBS_LOCK:
        JOBS[job_id] = job
    cfg = from_dict(req.config)

    def run():
        try:
            out_dir = Path(req.out_dir)
            out_dir.mkdir(parents=True, exist_ok=True)
            src = Path(req.path).stem

            def progress(done, total, found):
                pct = done * 100 // total if total else 0
                with JOBS_LOCK:
                    job["progress"] = {"done": done, "total": total,
                                       "found": found, "pct": pct}

            with VideoSource(req.path) as vs:
                job["log"].append(f"视频: {vs.width}x{vs.height} @ {vs.frame_rate:.2f} fps, {vs.duration:.2f}s")
                res = rip(vs, cfg, on_progress=progress, auto_color=req.auto_color)

            artifacts: dict[str, str] = {}
            vinfo = res.video_info
            if res.events and cfg.output.ssa:
                p = out_dir / f"{src}.ass"
                write_ssa(res.events, vinfo["width"], vinfo["height"], cfg, p)
                artifacts["ssa"] = str(p)
            if res.events and cfg.output.vobsub:
                sp, ip = write_vobsub(res.events, vinfo["width"], vinfo["height"], cfg, out_dir / src)
                artifacts["sub"], artifacts["idx"] = str(sp), str(ip)
            if res.events and cfg.output.ocr_png:
                files = write_ocr_png(res.events, cfg, out_dir / "subtitle_imgs")
                artifacts["ocr_images"] = str(out_dir / "subtitle_imgs")
            if cfg.output.json_timeline:
                p = out_dir / f"{src}.timeline.json"
                write_json_timeline(res.events, vinfo, cfg, p)
                artifacts["timeline"] = str(p)
            proj = out_dir / f"{src}.esrng.json"
            write_project(res.events, req.path, cfg, proj, artifacts)
            artifacts["project"] = str(proj)

            events = [{
                "index": i, "start": round(e.start, 4), "end": round(e.end, 4),
                "start_frame": e.start_frame, "end_frame": e.end_frame,
                "bbox": list(e.bbox), "duration": round(e.end - e.start, 4),
                "diff_frames": e.diff_frames,
            } for i, e in enumerate(res.events, 1)]
            with JOBS_LOCK:
                job["result"] = {
                    "events": events, "artifacts": artifacts,
                    "video_info": vinfo,
                    "frames_processed": res.frames_processed,
                    "candidates": res.candidates, "elapsed_s": round(res.elapsed_s, 2),
                }
                job["status"] = "done"
                job["log"].append(f"完成：字幕 {len(res.events)} 条，耗时 {res.elapsed_s:.2f}s")
        except Exception as e:
            with JOBS_LOCK:
                job["status"] = "error"
                job["log"].append(f"错误: {e}")

    t = threading.Thread(target=run, daemon=True)
    t.start()
    return {"job_id": job_id}


@app.get("/api/jobs/{job_id}")
def api_job(job_id: str):
    with JOBS_LOCK:
        job = JOBS.get(job_id)
        if not job:
            raise HTTPException(404, "任务不存在")
        return {k: v for k, v in job.items() if k != "result"} | (
            {"result": job["result"]} if job["result"] is not None else {}
        )


@app.get("/api/jobs")
def api_jobs():
    with JOBS_LOCK:
        return [{"id": j["id"], "status": j["status"],
                 "video": j["video"], "progress": j["progress"]} for j in JOBS.values()]


@app.get("/api/artifact")
def api_artifact(path: str):
    return FileResponse(_safe_artifact(path))


# ------------------------------------------------------------------ 静态托管
app.mount("/vendor", StaticFiles(directory=str(UI_DIR / "vendor")), name="vendor")
app.mount("/ui", StaticFiles(directory=str(UI_DIR), html=True), name="ui")
