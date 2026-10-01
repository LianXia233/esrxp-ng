// 临时候诊断：从 .esr 加载第一条事件，用 render_subtitle_tile 渲染并落盘，验证渲染器是否正确。
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let esr = args.get(1).map(String::as_str).unwrap_or("/workspace/rip_out/sample_hardsub.esr");
    let out_png = args.get(2).map(String::as_str).unwrap_or("/tmp/tile_check.png");

    let (events, _filtered, _video, cfg) = esrxp_ng_server::outputs::load_esr(Path::new(esr)).unwrap();
    let ev = &events[0];
    eprintln!("event0 bbox={:?} image_wxh={}x{} mask_density={:.1}%",
              ev.bbox, ev.image_w, ev.image_h,
              100.0 * ev.mask.iter().filter(|v| **v > 0).count() as f64 / (ev.mask.len().max(1)) as f64);

    // 用与 write_ocr_png 相同的渲染管线渲染此事件并保存
    let ocr = cfg.output.ocr.clone();
    let tile = esrxp_ng_server::outputs::render_subtitle_tile(ev, &ocr).expect("render");
    let _tile = esrxp_ng_server::outputs::apply_canvas_postprocess(
        esrxp_ng_server::outputs::apply_tile_postprocess(tile, &ocr), &ocr);
    _tile.save(out_png).unwrap();
    eprintln!("saved -> {out_png} size {}x{}", _tile.width(), _tile.height());
}