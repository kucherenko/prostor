//! Smoke test that runs natively (`cargo test`) and in headless Chrome
//! (`wasm-pack test --headless --chrome core/prostor-protocol`). Every protocol
//! test should follow this pattern so both runtimes stay covered.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

#[cfg(target_arch = "wasm32")]
wasm_bindgen_test_configure!(run_in_browser);

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn reports_its_version_in_both_runtimes() {
    assert_eq!(prostor_protocol::crate_version(), env!("CARGO_PKG_VERSION"));
}
