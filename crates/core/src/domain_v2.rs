//! Pure business domain. Authenticated adapters construct Actors; SQL owns persistence.
mod command;
mod model;
mod projection;
mod rules;
#[cfg(test)]
mod tests;
pub use command::*;
pub use model::*;
pub use projection::*;
