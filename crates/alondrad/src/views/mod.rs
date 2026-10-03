pub(crate) mod cache;
pub(crate) mod actions;
pub(crate) mod feed;
pub(crate) mod live;
pub(crate) mod reset_time;
pub(crate) mod routes;

pub(crate) const VIEW_BUDGET_BYTES: usize = {
    let local = crate::service::MAX_LOCAL_RESPONSE - 16 * 1024;
    let encrypted = (crate::service::MAX_RELAY_RESPONSE - 8 * 1024) * 3 / 4;
    if local < encrypted { local } else { encrypted }
};
