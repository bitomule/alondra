//! What the daemon hands the screens: the wire types of a view, the ids of its actions and what it
//! observes of running nodes. The engine writes these; `views` renders them.
pub mod accounts;
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
pub use live::*;
pub use text::Lang;
