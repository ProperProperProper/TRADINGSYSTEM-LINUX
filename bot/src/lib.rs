//! Mean-reversion paper bot for US stocks over moomoo OpenD, all Rust: the trading engine
//! (`engine`) on moomoo data, executed as real moomoo paper orders.
//!
//! * `pb` — OpenD's protocol messages, generated from moomoo's published .proto files.
//! * `opend` — the TCP client for the local OpenD gateway (framing, SHA-1 check, request/reply).
//! * `engine` — the trading engine (unchanged strategy) and the moomoo data / order layer.

#[allow(clippy::all, non_snake_case, non_camel_case_types, dead_code)]
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/pb.rs"));
}
pub mod engine;
pub mod opend;
/// OS differences (folders, probes, process checks): the same file as the lab's, mirrored in lab/ (see lab/src/platform.rs).
#[path = "../lab/src/platform.rs"]
pub mod platform;
