// ABOUTME: Runs explicit ALSA discovery, silent negotiation, and approved fixture exercises.
// ABOUTME: Emits JSON evidence without default-device selection or automatic recovery.
#![forbid(unsafe_code)]
use std::process::ExitCode;

#[cfg(target_os = "linux")]
mod cli;

fn main() -> ExitCode {
    #[cfg(target_os = "linux")]
    {
        cli::main()
    }
    #[cfg(not(target_os = "linux"))]
    {
        println!(
            "{}",
            serde_json::json!({"status":"fail","error":{"code":"CAPABILITY_UNSUPPORTED","message":"ALSA output requires Linux"}})
        );
        ExitCode::from(2)
    }
}
