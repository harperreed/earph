// ABOUTME: Launches the native Earph controller without creating an engine implicitly.
// ABOUTME: Leaves owner attachment and explicit launch to the visible Connect workflow.
fn main() -> eframe::Result {
    earph_desktop::gui::run()
}
