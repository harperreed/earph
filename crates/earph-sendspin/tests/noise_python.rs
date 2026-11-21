// ABOUTME: Qualifies real aiosendspin NoiseSession producer and consumer interoperability.
// ABOUTME: Requires an explicit pinned Python launcher and never uses operator credentials.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use earph_sendspin::noise::*;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
};

struct PeerChild(std::process::Child);
impl Drop for PeerChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn unhex(value: &Value) -> Vec<u8> {
    let (pairs, remainder) = value.as_str().unwrap().as_bytes().as_chunks::<2>();
    assert!(remainder.is_empty());
    pairs
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
fn hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn write(input: &mut impl Write, value: Value) {
    writeln!(input, "{value}").unwrap();
    input.flush().unwrap();
}
fn read(output: &mut impl BufRead) -> Value {
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
#[ignore = "requires EARPH_SENDSPIN_PEER pointing to a pinned aiosendspin 10.2.1 Python launcher"]
fn actual_python_noise_session_produces_and_consumes_records() {
    let launcher = std::env::var_os("EARPH_SENDSPIN_PEER").expect("set the pinned peer launcher");
    for mode in [
        "sn",
        "lt",
        "pr",
        "fallback",
        "wrong_psk",
        "prologue",
        "wrong_static",
        "corrupt",
        "replay",
        "order",
        "boundary",
    ] {
        let mut child = PeerChild(
            Command::new(&launcher)
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/support/noise_peer.py"
                ))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let mut input = child.0.stdin.take().unwrap();
        let mut output = BufReader::new(child.0.stdout.take().unwrap());
        let private = generate_static_private_key().unwrap();
        let public = static_public_key(&private).unwrap();
        let client_init = format!(
            "{{ \"type\":\"client/init\", \"payload\":{{\"version\":1,\"suite\":\"25519_ChaChaPoly_SHA256\",\"client_id\":\"{}\"}}}}",
            URL_SAFE_NO_PAD.encode(public)
        );
        write(
            &mut input,
            json!({"mode":mode,"client_public":URL_SAFE_NO_PAD.encode(public),"client_init":client_init}),
        );
        let first = read(&mut output);
        let server_init = first["server_init"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(server_init).unwrap();
        let server_public: [u8; 32] = URL_SAFE_NO_PAD
            .decode(parsed["payload"]["server_id"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let credentials = match mode {
            "lt" | "wrong_psk" => vec![Credential::LongTerm {
                psk: [7; 32],
                server_public,
            }],
            "pr" => vec![Credential::Pairing { psk: [7; 32] }],
            _ => vec![],
        };
        let mut responder = Responder::new(
            &private,
            client_init.as_bytes(),
            server_init.as_bytes(),
            Limits::default(),
        )
        .unwrap();
        let reply = responder.receive_handshake(&unhex(&first["handshake"]), &credentials);
        if matches!(mode, "prologue" | "wrong_static") {
            assert_eq!(reply, Err(ChannelError::Authentication));
            assert_eq!(responder.phase(), Phase::Closed);
            write(&mut input, json!({"abort":true}));
            assert_eq!(read(&mut output)["outcome"], "aborted");
        } else {
            write(&mut input, json!({"handshake":hex(&reply.unwrap())}));
            let records = read(&mut output);
            if mode == "wrong_psk" {
                assert_eq!(records["outcome"], "wrong_psk_rejected");
                assert_eq!(responder.phase(), Phase::PendingConfirmation);
                responder.close();
            } else {
                assert_eq!(records["outcome"], "transport");
                assert_eq!(
                    responder.handshake_hash().unwrap().as_slice(),
                    unhex(&records["handshake_hash"])
                );
                if mode == "fallback" {
                    assert_eq!(
                        responder.selection(),
                        Some(Selection {
                            category: Category::Sentinel,
                            sentinel_fallback: true
                        })
                    );
                }
                let record = unhex(&records["record"]);
                if matches!(mode, "corrupt" | "order") {
                    let bad = if mode == "order" {
                        unhex(&records["next"])
                    } else {
                        record
                    };
                    assert_eq!(responder.decrypt(&bad), Err(ChannelError::Authentication));
                    assert_eq!(responder.phase(), Phase::Closed);
                    write(&mut input, json!({"abort":true}));
                    assert_eq!(read(&mut output)["outcome"], "aborted");
                } else {
                    let expected = if mode == "boundary" {
                        vec![4; 65519]
                    } else {
                        b"\x04python-server".to_vec()
                    };
                    assert_eq!(responder.decrypt(&record).unwrap(), expected);
                    if mode == "replay" {
                        assert_eq!(
                            responder.decrypt(&record),
                            Err(ChannelError::Authentication)
                        );
                        assert_eq!(responder.phase(), Phase::Closed);
                        write(&mut input, json!({"abort":true}));
                        assert_eq!(read(&mut output)["outcome"], "aborted");
                    } else {
                        let plaintext = if mode == "boundary" {
                            vec![4; 65519]
                        } else {
                            b"\x04rust-client".to_vec()
                        };
                        write(
                            &mut input,
                            json!({"record":hex(&responder.encrypt(&plaintext).unwrap())}),
                        );
                        assert_eq!(read(&mut output)["outcome"], "both_directions_verified");
                    }
                }
            }
        }
        drop(input);
        assert!(
            child.0.wait().unwrap().success(),
            "Python peer failed in {mode}"
        );
        eprintln!("real pinned Python peer mode {mode}: passed");
    }
}
