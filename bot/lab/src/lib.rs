//! Qwen Lab: a standalone research lab in which a local Qwen model proposes hypotheses about a trading strategy and
//! deterministic code judges them. See README.md and STATE.md.
// Style lints kept off deliberately: statistics code indexes several arrays in step, store helpers take many columns, row tuples
// mirror SQL selects, and `!(x > 0.0)` is intentional (it also rejects NaN).
// the dashboard snapshot is one large json! literal
#![recursion_limit = "256"]
#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::needless_range_loop, clippy::neg_cmp_op_on_partial_ord, clippy::explicit_counter_loop)]

pub mod action_items;
pub mod agents;
pub mod announce;
pub mod bot_status;
pub mod brain;
pub mod brain_agents;
pub mod botctl;
pub mod brain_roles;
pub mod online_brain;
pub mod btarchive;
pub mod config;
pub mod db;
pub mod contract;
pub mod cull;
pub mod dash;
pub mod facts;
pub mod quant;
pub mod fix_report;
pub mod fn_docs;
pub mod guard;
pub mod labtools;
pub mod highlights;
pub mod holdings_xlsx;
pub mod ingest;
pub mod misc_tools;
pub mod candles;
pub mod mlbot;
pub mod automl;
pub mod deepnet;
pub mod indicators;
pub mod autodiff;
pub mod portfolio;
pub mod intel;
pub mod aimodel;
pub mod mlcandle;
pub mod mlcandle2;
pub mod mlfeat;
pub mod mlimprove;
pub mod mlmeta;
pub mod mlround;
pub mod mlscores;
pub mod mlwalk;
pub mod mltrade;
pub mod notify;
pub mod monitor;
pub mod ops;
pub mod opsctl;
pub mod context;
pub mod paper;
pub mod paper_moomoo;
pub mod promote;
pub mod report;
pub mod schedule;
pub mod scout;
pub mod stats;
pub mod strategy;
pub mod supervisor;
pub mod tick;
pub mod teacher_engine;
pub mod jobs;
pub mod platform;
pub mod service;
pub mod sysinfo;
#[cfg(test)]
pub mod synth;
pub mod util;
pub mod warnings_ledger;
pub mod webhook_guard;
pub mod winrate;

#[cfg(test)]
pub mod testutil;
