//! esrxp-ng-server 库目标：暴露引擎模块供 examples / 测试复用（与 main.rs 共享同一套源码文件）。

pub mod api;
pub mod config;
pub mod filter;
pub mod gpu;
pub mod logging;
pub mod outputs;
pub mod postprocess;
pub mod ripper;
pub mod video;
