//! Minimal CSR/WASM UI boundary.
//!
//! This crate deliberately owns no provider key, filesystem, process, or
//! database access. It will call the Host HTTP/SSE protocol as the remaining
//! pages move over from the legacy HTML/JavaScript fallback.

use leptos::prelude::*;
use peachsh_protocol::SessionKind;

#[component]
pub fn App() -> impl IntoView {
    let mode = SessionKind::Chat.as_str();
    view! {
        <main class="peachsh-ui">
            <h1>"🍑sh harness"</h1>
            <p>"Rust + Leptos CSR"</p>
            <p class="session-kind">"当前会话协议：" {mode}</p>
        </main>
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    leptos::mount::mount_to_body(App);
}
