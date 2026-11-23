// ABOUTME: Checks the pinned version-zero token format with public synthetic credentials.
// ABOUTME: Exercises operator input leniency without logging exported credentials.
use earph_sendspin::pairing_token::*;

const VECTOR: &str = "SP:0AAAQEAYEAUDAOCAJBIFQYDIOB4IBCEQTCQKRMFYYDENBWHA5DYP6BYPC4PSOLZXH5DU6V97M5XXO74HR6LZ7J5PW674PT6X37T6757Y";

fn token() -> PairingToken {
    PairingToken {
        client_public: std::array::from_fn(|i| i as u8),
        pairing_psk: std::array::from_fn(|i| 0xe0 + i as u8),
    }
}

#[test]
fn published_version_zero_vector() {
    assert_eq!(encode_pairing_token(&token()), VECTOR);
    assert_eq!(decode_pairing_token(VECTOR).unwrap(), token());
}

#[test]
fn operator_leniency_and_extensions() {
    for text in [
        VECTOR.to_lowercase(),
        format!(" \t{VECTOR}\r\n"),
        VECTOR[3..].to_owned(),
        VECTOR.replace('9', "2"),
    ] {
        assert_eq!(decode_pairing_token(&text).unwrap(), token());
    }
    // The 64-byte payload ends on a partial symbol. Appending five zero bits supplies one extension byte.
    assert_eq!(
        decode_pairing_token(&format!("{VECTOR}A")).unwrap(),
        token()
    );
}

#[test]
fn malformed_input_and_wrong_versions_are_rejected() {
    for text in [
        "",
        "SP:",
        "SP:1AAAA",
        "SP:14DQ6FY7E4XTOP9HJ5LV6Z3PO57YPD4XT6T97N5Y",
        "SP:7AAAA",
        "SP:0AAAA",
        "SP:0ß",
        "SP:0AAAA-AAA",
        "SP:0AAAA AAA",
    ] {
        assert!(
            decode_pairing_token(text).is_err(),
            "accepted malformed token"
        );
    }
    for character in ['0', '1', '8', ':', '-', ' ', 'é'] {
        let mut malformed = VECTOR.to_owned();
        malformed.replace_range(20..21, &character.to_string());
        assert!(decode_pairing_token(&malformed).is_err());
    }
}

#[test]
fn diagnostics_redact_the_credential() {
    assert_eq!(format!("{:?}", token()), "PairingToken([REDACTED])");
    let error = decode_pairing_token("secret").unwrap_err();
    assert!(!format!("{error:?} {error}").contains("secret"));
}
