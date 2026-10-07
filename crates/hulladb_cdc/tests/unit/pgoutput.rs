use super::*;

fn begin() -> Vec<u8> {
    let mut data = vec![b'B'];
    data.extend(100_u64.to_be_bytes());
    data.extend(0_u64.to_be_bytes());
    data.extend(1_u32.to_be_bytes());
    data
}
fn relation() -> Vec<u8> {
    let mut data = vec![b'R'];
    data.extend(7_u32.to_be_bytes());
    data.extend(b"public\0tasks\0");
    data.push(b'd');
    data.extend(1_u16.to_be_bytes());
    data.push(1);
    data.extend(b"id\0");
    data.extend(25_u32.to_be_bytes());
    data.extend(u32::MAX.to_be_bytes());
    data
}
fn insert() -> Vec<u8> {
    let mut data = vec![b'I'];
    data.extend(7_u32.to_be_bytes());
    data.push(b'N');
    data.extend(1_u16.to_be_bytes());
    data.push(b't');
    data.extend(3_u32.to_be_bytes());
    data.extend(b"abc");
    data
}
fn commit() -> Vec<u8> {
    let mut data = vec![b'C', 0];
    data.extend(100_u64.to_be_bytes());
    data.extend(101_u64.to_be_bytes());
    data.extend(0_u64.to_be_bytes());
    data
}
#[test]
fn complete_transaction_and_truncated_messages() {
    let mut decoder = Decoder::default();
    assert!(matches!(
        decoder.push(&begin()).unwrap(),
        Some(DecodedEvent::Begin {
            xid: 1,
            commit: Lsn(100)
        })
    ));
    assert!(decoder.push(&relation()).unwrap().is_none());
    assert!(matches!(
        decoder.push(&insert()).unwrap(),
        Some(DecodedEvent::Change(Change::Insert { .. }))
    ));
    assert!(matches!(
        decoder.push(&commit()).unwrap(),
        Some(DecodedEvent::Commit {
            commit: Lsn(100),
            end: Lsn(101)
        })
    ));
    assert!(!decoder.in_transaction());
    for message in [begin(), relation(), insert(), commit()] {
        for length in 0..message.len() {
            let mut decoder = Decoder::default();
            if message[0] != b'B' {
                decoder.push(&begin()).unwrap();
            }
            decoder.push(&relation()).unwrap();
            assert!(decoder.push(&message[..length]).is_err());
        }
    }
}
#[test]
fn refuse_gaps_unknown_messages_nested_transactions_and_trailing_bytes() {
    assert!(Decoder::default().push(&insert()).is_err());
    assert!(Decoder::default().push(&commit()).is_err());
    assert!(Decoder::default().push(b"?").is_err());
    let mut decoder = Decoder::default();
    decoder.push(&begin()).unwrap();
    assert!(decoder.push(&begin()).is_err());
    let mut decoder = Decoder::default();
    decoder.push(&begin()).unwrap();
    decoder.push(&relation()).unwrap();
    let mut malformed = insert();
    malformed.push(0);
    assert!(decoder.push(&malformed).is_err());
}

#[test]
fn tuple_null_toast_binary_and_count_limits() {
    let mut reader = Reader(&[0, 2, b'n', b'u']);
    assert_eq!(
        reader.tuple(2).unwrap(),
        vec![Value::Null, Value::UnchangedToast]
    );
    assert!(Reader(&[0, 2, b'n', b'u']).tuple(1).is_err());
    assert!(Reader(&[0, 1, b'b', 0, 0, 0, 1, 0]).tuple(1).is_err());
    assert!(Reader(&[0, 1, b't', 255, 255, 255, 255]).tuple(1).is_err());
    assert!(matches!(
        Decoder::default().push(&vec![0; MAX_MESSAGE_BYTES + 1]),
        Err(Error::Limit)
    ));
}

#[test]
fn ignored_logical_messages_are_not_accumulated_but_still_validate_framing() {
    let mut decoder = Decoder::default();
    assert!(decoder.ignore_message(true, 1).is_err());
    assert!(matches!(
        decoder.ignore_message(false, MAX_MESSAGE_BYTES + 1),
        Err(Error::Limit)
    ));
    decoder.ignore_message(false, MAX_MESSAGE_BYTES).unwrap();
    decoder.push(&begin()).unwrap();
    assert!(decoder.in_transaction());
    for _ in 0..32 {
        decoder.ignore_message(true, MAX_MESSAGE_BYTES).unwrap();
    }
    assert!(matches!(
        decoder.push(&commit()).unwrap(),
        Some(DecodedEvent::Commit { .. })
    ));
}
