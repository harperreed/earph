// ABOUTME: Generates pinned native bindings from the actual compiled FFI library.
// ABOUTME: Keeps generator-only dependencies behind the explicit bindgen feature.
fn main() {
    uniffi::uniffi_bindgen_main();
}
