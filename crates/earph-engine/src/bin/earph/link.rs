// ABOUTME: Connects the player CLI to one Engine owner, hosting that owner when none runs.
// ABOUTME: Fences every command with fresh owner state and reports refusals in plain words.
use earph_core::ErrorCode;
use earph_engine::{DeviceDiscoveryHelper, EngineOwner};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// One user-facing failure with its process exit code.
#[derive(Debug)]
pub struct Failure {
    pub exit: u8,
    pub message: String,
}
impl Failure {
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            exit: 2,
            message: message.into(),
        }
    }
    pub fn engine(message: impl Into<String>) -> Self {
        Self {
            exit: 3,
            message: message.into(),
        }
    }
    pub fn playback(message: impl Into<String>) -> Self {
        Self {
            exit: 1,
            message: message.into(),
        }
    }
}
pub type Outcome<T> = Result<T, Failure>;

/// Which session a command targets.
pub enum Target<'a> {
    None,
    Current,
    New(&'a str),
}

pub struct Link {
    runtime: PathBuf,
    hosted: Option<JoinHandle<Result<(), earph_core::EngineError>>>,
    next: u64,
}
impl Link {
    /// Attaches to the owner at `runtime`. With `host`, starts an owner here when none runs.
    pub fn open(runtime: &Path, host: bool) -> Outcome<Self> {
        let mut link = Self {
            runtime: runtime.to_owned(),
            hosted: None,
            next: 0,
        };
        if host {
            match EngineOwner::bind(runtime) {
                Ok(owner) => {
                    let executable = std::env::current_exe().map_err(|error| {
                        Failure::engine(format!("cannot locate this executable: {error}"))
                    })?;
                    let helper = DeviceDiscoveryHelper::from_executable(&executable)
                        .map_err(|error| Failure::engine(describe(&error)))?;
                    let owner = owner.with_device_discovery(helper);
                    link.hosted = Some(thread::spawn(move || owner.serve()));
                }
                Err(error) if error.code == ErrorCode::SourceBusy => {}
                Err(error) => {
                    return Err(Failure::engine(format!(
                        "cannot start an Engine at {}: {}",
                        runtime.display(),
                        describe(&error)
                    )));
                }
            }
        }
        link.state().map_err(|failure| {
            if host {
                failure
            } else {
                Failure::engine(format!(
                    "no Engine is running at {}; start one with `earph play`",
                    runtime.display()
                ))
            }
        })?;
        Ok(link)
    }
    pub fn hosted(&self) -> bool {
        self.hosted.is_some()
    }
    pub fn runtime(&self) -> &Path {
        &self.runtime
    }
    fn request_id(&mut self) -> String {
        self.next += 1;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        format!("cli-{:x}-{nanos:x}-{:x}", std::process::id(), self.next)
    }
    /// Builds one envelope. Resend the same bytes to read the same receipt again.
    pub fn envelope(
        &mut self,
        owner: Option<&str>,
        session: Option<&str>,
        revision: Option<u64>,
        payload: Value,
    ) -> Vec<u8> {
        json!({
            "schema_version": 1,
            "request_id": self.request_id(),
            "zone_id": "main",
            "owner_id": owner,
            "session_id": session,
            "expected_revision": revision,
            "payload": payload,
        })
        .to_string()
        .into_bytes()
    }
    /// Sends exact envelope bytes and returns the receipt.
    pub fn exchange(&self, envelope: &[u8]) -> Outcome<Value> {
        let reply = earph_engine::request(&self.runtime, envelope)
            .map_err(|error| Failure::engine(describe(&error)))?;
        let mut document: Value = serde_json::from_slice(&reply)
            .map_err(|_| Failure::engine("the Engine returned an unreadable reply"))?;
        if let Some(error) = document.get("error").filter(|error| !error.is_null()) {
            return Err(Failure::engine(format!(
                "{}: {}",
                text(&error["code"]),
                text(&error["message"])
            )));
        }
        Ok(document["receipt"].take())
    }
    pub fn query(&mut self, payload: Value) -> Outcome<Value> {
        let envelope = self.envelope(None, None, None, payload);
        let mut receipt = self.exchange(&envelope)?;
        accepted_or_applied(&receipt, "query")?;
        Ok(receipt["result"]["value"].take())
    }
    pub fn state(&mut self) -> Outcome<Value> {
        self.query(json!({"method": "state.get"}))
    }
    /// Sends one mutation against fresh state. A changed revision earns a fresh attempt.
    pub fn mutate(&mut self, payload: Value, target: Target<'_>) -> Outcome<(Value, Vec<u8>)> {
        let method = text(&payload["method"]);
        for _ in 0..8 {
            let state = self.state()?;
            let session = match target {
                Target::None => None,
                Target::Current => state["session_id"].as_str(),
                Target::New(id) => Some(id),
            };
            let envelope = self.envelope(
                state["owner_id"].as_str(),
                session,
                state["revision"].as_u64(),
                payload.clone(),
            );
            let receipt = self.exchange(&envelope)?;
            if receipt["error"]["code"] == "STALE_REVISION" {
                continue;
            }
            accepted_or_applied(&receipt, &method)?;
            return Ok((receipt, envelope));
        }
        Err(Failure::engine(format!(
            "{method}: Engine state kept changing; try again"
        )))
    }
    /// Sends one mutation and waits until the Engine finishes it.
    /// An accepted transport change completes only when the playback worker acknowledges it.
    pub fn complete(&mut self, payload: Value, target: Target<'_>) -> Outcome<()> {
        let method = text(&payload["method"]);
        let (mut receipt, envelope) = self.mutate(payload, target)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while receipt["status"] == "accepted" {
            if Instant::now() >= deadline {
                return Err(Failure::engine(format!(
                    "{method}: the Engine accepted the request but did not finish it"
                )));
            }
            thread::sleep(Duration::from_millis(20));
            receipt = self.exchange(&envelope)?;
        }
        accepted_or_applied(&receipt, &method)
    }
    /// Repeats `read` until `done` returns a value or the limit passes.
    pub fn wait<T>(
        &mut self,
        limit: Duration,
        what: &str,
        mut done: impl FnMut(&mut Self) -> Outcome<Option<T>>,
    ) -> Outcome<T> {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(value) = done(self)? {
                return Ok(value);
            }
            if Instant::now() >= deadline {
                return Err(Failure::engine(format!("timed out waiting for {what}")));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    /// Stops a hosted owner and waits for it. An attached owner keeps running.
    pub fn close(mut self) -> Outcome<()> {
        let Some(join) = self.hosted.take() else {
            return Ok(());
        };
        self.mutate(json!({"method": "owner.shutdown"}), Target::Current)?;
        join.join()
            .map_err(|_| Failure::engine("the hosted Engine panicked"))?
            .map_err(|error| Failure::engine(describe(&error)))
    }
}

fn accepted_or_applied(receipt: &Value, method: &str) -> Outcome<()> {
    if matches!(receipt["status"].as_str(), Some("accepted" | "applied")) {
        return Ok(());
    }
    Err(Failure::engine(format!(
        "{method} was {}: {} ({})",
        text(&receipt["status"]),
        text(&receipt["error"]["message"]),
        text(&receipt["error"]["code"])
    )))
}
pub fn text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}
fn describe(error: &earph_core::EngineError) -> String {
    let document = serde_json::to_value(error).unwrap_or(Value::Null);
    format!(
        "{}: {}",
        text(&document["code"]),
        text(&document["message"])
    )
}
