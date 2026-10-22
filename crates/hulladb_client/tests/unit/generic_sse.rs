use super::*;
fn frames(bytes: &[u8], limit: usize) -> Result<Vec<SseFrame>, Error> {
    let mut decoder = SseDecoder::new(limit);
    let mut frames = vec![];
    for byte in bytes {
        if let Some(frame) = decoder.byte(*byte)? {
            frames.push(frame);
        }
    }
    Ok(frames)
}
#[test]
fn sse_handles_fragmented_utf8_mixed_delimiters_multiline_data_and_comments() {
    let bytes=b": ping\r\nevent: snapshot\rid: origin:1\ndata: h\xc3\xa9\r\ndata: there\r\n\r\n: heartbeat\n\ndata: next\n\n";
    let events = frames(bytes, 128).unwrap();
    assert_eq!(events.len(), 3);
    assert!(
        matches!(&events[0],SseFrame::Event{name,id:Some(id),data} if name=="snapshot" && id=="origin:1" && data=="hé\nthere")
    );
    assert!(matches!(&events[1], SseFrame::Heartbeat));
    assert!(
        matches!(&events[2],SseFrame::Event{name,id:None,data} if name=="message" && data=="next")
    );
}
#[test]
fn sse_bounds_entire_frames_and_rejects_invalid_utf8() {
    assert!(matches!(
        frames(b"data: a\ndata: b\n\n", 16),
        Err(Error::Budget)
    ));
    assert!(matches!(
        frames(b"data: a\r\ndata: b\r\n\r\n", 18),
        Err(Error::Budget)
    ));
    assert!(matches!(
        frames(b"data: \xff\n\n", 100),
        Err(Error::Protocol)
    ));
    assert!(frames(b"data: a\n\ndata: b\n\n", 9).is_ok());
}

#[test]
fn intentional_errors_preserve_payload_without_becoming_transport_control_codes() {
    let body = br#"{"code":"execution_failed","application":{"code":"unauthorized","message":"Task locked","details":null}}"#;
    let error = checked_error(StatusCode::UNPROCESSABLE_ENTITY, body).unwrap();
    assert!(
        matches!(error, Error::Application(error) if error.code == "unauthorized" && error.message == "Task locked" && error.details == Some(serde_json::Value::Null))
    );
    assert!(matches!(
        checked_error(StatusCode::UNAUTHORIZED, body),
        Err(Error::Protocol)
    ));
    let malformed = br#"{"code":"unauthorized","application":{"code":"X","message":"private"}}"#;
    assert!(matches!(
        checked_error(StatusCode::UNAUTHORIZED, malformed),
        Err(Error::Protocol)
    ));
    let excessive = serde_json::json!({"code":"execution_failed","application":{"code":"X","message":"x".repeat(16*1024)}});
    assert!(matches!(
        checked_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            excessive.to_string().as_bytes()
        ),
        Err(Error::Protocol)
    ));
}
