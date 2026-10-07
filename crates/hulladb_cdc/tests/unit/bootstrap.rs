use super::*;

#[tokio::test]
async fn bootstrap_frames_reject_oversized_and_truncated_inputs_before_allocation() {
    for length in [-1, 3, (MAX_FRAME_BYTES + 1) as i32] {
        let mut bytes = vec![b'D'];
        bytes.extend(length.to_be_bytes());
        assert!(matches!(
            read_frame(&mut bytes.as_slice()).await,
            Err(Error::Protocol(_))
        ));
    }
    assert!(
        read_frame(&mut [b'D', 0, 0, 0, 8, 1].as_slice())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_ready_frame_cannot_skip_bootstrap_authentication() {
    let (socket, mut server) = tokio::io::duplex(32);
    use tokio::io::AsyncWriteExt;
    server.write_all(&[b'Z', 0, 0, 0, 5, b'I']).await.unwrap();
    let mut connection = ExportConnection {
        socket: Box::new(socket),
    };
    assert!(matches!(
        connection.authenticate(&ReplicationConfig::default()).await,
        Err(Error::Protocol(_))
    ));
}

#[test]
fn export_rows_reject_nulls_truncation_extra_fields_and_unsafe_snapshot_names() {
    let mut row = vec![0, 1];
    row.extend(3_i32.to_be_bytes());
    row.extend(b"abc");
    assert_eq!(parse_row(&row, 1).unwrap(), ["abc"]);
    for length in 0..row.len() {
        assert!(parse_row(&row[..length], 1).is_err());
    }
    let mut trailing = row.clone();
    trailing.push(0);
    assert!(parse_row(&trailing, 1).is_err());
    assert!(parse_row(&[0, 1, 255, 255, 255, 255], 1).is_err());
    assert!(parse_row(&row, 2).is_err());
    assert!(valid_snapshot_name("00000003-0000001B-1"));
    for name in ["", "snapshot';SELECT 1", "a\\b", "é"] {
        assert!(!valid_snapshot_name(name));
    }
}
