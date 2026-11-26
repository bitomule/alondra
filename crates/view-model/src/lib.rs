//! What the daemon hands the screens: the wire types of a view, the ids of its actions and what it
//! observes of running nodes. The engine writes these; `views` renders them.
pub mod accounts;
pub mod action_outcome;
pub mod capabilities;
pub mod messages;
pub mod metrics;
pub mod model_controls;
pub mod resolves;
pub mod review_export;
pub mod screens;
pub mod spec_sheet;
pub mod action_id;
pub mod command_line;
pub mod components;
pub mod live;
pub mod mcp;
pub mod now_fact;
pub mod pr_images;
pub mod refusal;
pub mod text;

pub use components::*;
pub use spec_sheet::*;
pub use live::*;
pub use text::Lang;
pub use screens::*;
pub use capabilities::{ClientCapabilities, InvalidCapabilities};
pub use messages::{Outcome, OutcomeKind, refusal_text, scheduled_message_stamp, send_anyway, transport_failure, unknown_outcome};
pub use resolves::{pr_presentation, resolves_card};
