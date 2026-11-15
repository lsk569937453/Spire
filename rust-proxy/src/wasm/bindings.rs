//! Component-model bindings, generated from `wit/spire-filter.wit`.
//!
//! `bindgen!` validates the WIT at compile time and generates:
//!
//! * `bindings::spire::filter::{types, host}` — the types the guest receives and
//!   the `host::Host` trait this crate implements (in [`super::state`]);
//! * `bindings::exports::spire::filter::{lifecycle, request_filter,
//!   response_filter}` — the guest's `Guest` traits, which the host *calls*;
//! * `bindings::FilterPlugin{,Pre,Indices}` — the world, i.e. "a component that
//!   implements `filter-plugin`", with `add_to_linker` / `instantiate` /
//!   `instantiate_pre` on the world type.
//!
//! Deliberately no async: every callback is synchronous and bounded by fuel and
//! an epoch deadline (see [`super::engine`]). An `async` component-model host
//! would turn every call into a poll loop and buy nothing, because a gateway
//! needs a *deadline*, not a suspension point.
wasmtime::component::bindgen!({
    path: "wit/spire-filter.wit",
    world: "filter-plugin",
});
