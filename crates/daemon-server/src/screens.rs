//! The screens the clients read, asked of the implementation the root registered for the port.
use alondra_engine_ports::Registry;
use alondra_view_model::Screens;
use std::sync::{Arc, OnceLock};

static SCREENS: OnceLock<Arc<dyn Screens>> = OnceLock::new();

/// Takes the screens from the registry the root wired; the first daemon of the process sets them.
pub fn install(registry: &Registry) {
    if let Some(screens) = registry.port::<dyn Screens>() {
        let _ = SCREENS.set(screens);
    }
}

pub fn get() -> &'static dyn Screens {
    SCREENS.get().map_or_else(unregistered, |screens| screens.as_ref())
}

#[cfg(not(test))]
fn unregistered() -> &'static dyn Screens {
    panic!("the root registers the screens before the server renders one")
}

/// The server's own tests open runtimes without the root, so they render with the real screens.
#[cfg(test)]
fn unregistered() -> &'static dyn Screens {
    &alondra_views::Views
}
