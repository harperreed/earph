// ABOUTME: Implements controlled credential helper responses for process tests.
// ABOUTME: Uses only private synthetic fixtures and never invokes a native provider.
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};
pub fn run() {
    let mut head = [0; 4];
    std::io::stdin().read_exact(&mut head).unwrap();
    let mut body = vec![0; u32::from_be_bytes(head) as usize];
    std::io::stdin().read_exact(&mut body).unwrap();
    let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let address = request["config"]["SecretService"]["address"]
        .as_str()
        .unwrap();
    let mut parts = address.splitn(3, ':');
    assert_eq!(parts.next(), Some("fixture"));
    let case = parts.next().unwrap();
    let root = std::path::Path::new(parts.next().unwrap());
    let write = request["action"].get("Commit").is_some();
    if write {
        std::fs::write(root.join(format!("{case}-commit")), b"issued").unwrap();
    }
    if (case.starts_with("gated") && write) || case == "gated-load" {
        std::fs::write(root.join(format!("{case}-entered")), b"entered").unwrap();
        let end = Instant::now() + Duration::from_secs(4);
        while !root.join(format!("{case}-release")).is_file() {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let error = match case
        .strip_prefix("commit-")
        .filter(|_| write)
        .unwrap_or(if write { "good" } else { case })
    {
        "missing" => Some("Missing"),
        "locked" => Some("Locked"),
        "unavailable" => Some("Unavailable"),
        "ambiguous" => Some("Ambiguous"),
        "invalid-error" => Some("InvalidDocument"),
        "cancelled" => Some("Cancelled"),
        _ => None,
    };
    let document = if case.contains("transport") {
        earph_sendspin::credentials::CommittedCredentials::new(
            [5; 32],
            [7; 32],
            earph_sendspin::pairing::RecordCatalog::new(5).unwrap(),
            Some([41; 32]),
        )
        .unwrap()
    } else {
        super::document()
    };
    let result = if let Some(error) = error {
        serde_json::json!({"Err":error})
    } else if case == "invalid" {
        serde_json::json!({"Ok":[1,2,3]})
    } else if case.contains("uncertain") && write {
        serde_json::json!({"Err":"Uncertain"})
    } else if case == "mismatch" && write {
        serde_json::json!({"Ok":document.encode_secret().unwrap().as_secret_bytes()})
    } else if write {
        let bytes = serde_json::to_vec(&request["action"]["Commit"]).unwrap();
        std::fs::write(root.join(format!("{case}-document")), bytes).unwrap();
        serde_json::json!({"Ok":request["action"]["Commit"]})
    } else if root.join(format!("{case}-document")).is_file() {
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join(format!("{case}-document"))).unwrap())
                .unwrap();
        serde_json::json!({"Ok":value})
    } else {
        serde_json::json!({"Ok":document.encode_secret().unwrap().as_secret_bytes()})
    };
    let bytes = serde_json::to_vec(
        &serde_json::json!({"version":1,"operation":request["operation"],"result":result}),
    )
    .unwrap();
    std::io::stdout()
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    std::io::stdout().write_all(&bytes).unwrap();
}
