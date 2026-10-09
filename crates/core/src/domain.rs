//! Pure business domain. Authenticated adapters construct Actors; SQL owns persistence.
mod attention;
mod command;
mod model;
mod ownership;
mod plan;
mod projection;
mod refusal;
pub mod reads;
mod rules;
mod schedule;
#[cfg(test)]
mod attention_tests;
#[cfg(test)]
mod card_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod ownership_tests;
pub use attention::*;
pub use command::*;
pub use model::*;
pub use ownership::*;
pub use plan::*;
pub use projection::*;
pub use refusal::*;
pub use schedule::*;
pub use rules::{ALONDRA_MERGES, ENDED_UNANSWERED_PREFIX, cancelled_notice, delivery_withdrawn_for_questions, delivery_withdrawn_behind_pr, dependencies_done_notice, cleanup_request, merge_failed_text, merge_failure, merged_notice};
