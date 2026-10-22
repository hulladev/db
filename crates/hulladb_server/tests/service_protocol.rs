//! Private launcher failures must neither echo credentials nor accept an auth
//! reply as initialization. Successful serving is covered by the generated app.
#![cfg(feature = "typescript")]

use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn rejected(frame: Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hulladb-service"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{frame}").unwrap();
    // Keep the command stream open until the failure frame is emitted: otherwise
    // a valid init racing EOF intentionally exits quietly before startup.
    let output = child.wait_with_output().unwrap();
    drop(stdin);
    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["type"], "error");
    value
}

#[test]
fn startup_errors_do_not_echo_credentials_or_accept_auth_as_init() {
    let marker = "secret-credential-must-not-be-printed";
    let malformed = rejected(json!({"type":"init", "url":marker}));
    assert_eq!(malformed["message"], "invalid service command");
    assert!(!malformed.to_string().contains(marker));

    let unsolicited = rejected(json!({"type":"authenticated", "id":1, "context":{
        "principal":"00000000-0000-0000-0000-000000000000",
        "authorizationScope":"00000000-0000-0000-0000-000000000000",
        "identity":{"token":marker}
    }}));
    assert_eq!(unsolicited["message"], "service init must be first");
    assert!(!unsolicited.to_string().contains(marker));

    let invalid_worker = rejected(json!({"type":"init", "url":marker, "namespace":"app",
        "schema":{}, "manifest":"{}", "host":"127.0.0.1", "port":0, "origins":[],
        "worker":{"executable":"relative-bun", "entrypoint":"relative-worker.ts"}
    }));
    assert_eq!(invalid_worker["message"], "worker paths must be absolute");
    assert!(!invalid_worker.to_string().contains(marker));
}
