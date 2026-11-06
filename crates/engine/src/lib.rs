//! The bot's trading engine as used by ThomasAi. Re-exports only; nothing is re-implemented here.
pub use moomoo_bot::engine;

/// Git commit of the bot code this build links against (the bot embeds it in every archived run).
pub fn bot_commit() -> &'static str {
    engine::ENGINE_COMMIT
}

#[cfg(test)]
mod tests {
    #[test]
    fn links_against_the_real_bot_and_reports_its_commit() {
        let c = super::bot_commit();
        assert!(
            !c.is_empty() && c != "unknown",
            "bot commit not embedded: {c}"
        );
    }
}
pub mod ane_accelerator;
pub mod apple_teacher;
pub mod cognitive_researcher;
pub mod darwin_qos;
pub mod gbdt;
pub mod guards;
pub mod improve;
pub mod learner;
pub mod market;
pub mod model_decision;
pub mod momentum;
pub mod neural;
pub mod paper;
pub mod planner_gate;
pub mod polymath;
pub mod registry;
pub mod replay;
pub mod search;
pub mod sessions;
pub mod universe;
pub mod walkfwd;
pub mod signals;
pub mod market_replicator;
