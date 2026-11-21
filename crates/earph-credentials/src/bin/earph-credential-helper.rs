// ABOUTME: Runs one native credential request over anonymous standard pipes.
// ABOUTME: Emits no raw native errors, paths, account details, or secret diagnostics.
fn main() {
    if std::env::args().count() != 2
        || std::env::args().nth(1).as_deref() != Some("credential-helper")
        || earph_credentials::run_helper(
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
        )
        .is_err()
    {
        std::process::exit(1);
    }
}
