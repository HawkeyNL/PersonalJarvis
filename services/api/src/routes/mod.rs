//! HTTP route handlers, grouped by concern. Each submodule owns a cohesive slice
//! of the API surface (its handlers and request DTOs); the router is still wired
//! up centrally in [`crate::build_router`]. Handlers are `pub(crate)` so the
//! router can name them while keeping them off the public crate API.

pub(crate) mod auth;
pub(crate) mod broker;
pub(crate) mod portfolio;
pub(crate) mod voice;
