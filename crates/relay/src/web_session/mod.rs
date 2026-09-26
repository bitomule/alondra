//! Same-origin browser sessions. Browser code receives an opaque HttpOnly cookie
//! and a CSRF token, never OIDC access/refresh tokens or device content keys.
pub mod store;
pub mod oidc;
