use covenant_inference_node::frame::{read_frame, write_frame, FrameError, MAX_FRAME_BYTES};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Sample {
    name: String,
    count: u32,
}

#[tokio::test]
async fn frame_round_trips_through_a_pipe() {
    let (mut writer, mut reader) = tokio::io::duplex(64 * 1024);
    let value = Sample {
        name: "inference".to_owned(),
        count: 7,
    };
    write_frame(&mut writer, &value).await.unwrap();
    let decoded: Sample = read_frame(&mut reader).await.unwrap();
    assert_eq!(decoded, value);
}

#[tokio::test]
async fn oversize_payload_is_rejected_on_write() {
    let (mut writer, _reader) = tokio::io::duplex(64 * 1024);
    let value = Sample {
        name: "x".repeat(MAX_FRAME_BYTES),
        count: 0,
    };
    let error = write_frame(&mut writer, &value).await.unwrap_err();
    assert!(matches!(error, FrameError::Oversize(_)));
}

#[tokio::test]
async fn oversize_length_prefix_is_rejected_before_allocating() {
    let (mut writer, mut reader) = tokio::io::duplex(64);
    // Write only the length prefix: a sound reader must reject it on the
    // advertised size, without waiting for or allocating the body it claims.
    writer.write_u32(MAX_FRAME_BYTES as u32 + 1).await.unwrap();
    writer.flush().await.unwrap();
    let error = read_frame::<Sample, _>(&mut reader).await.unwrap_err();
    assert!(matches!(error, FrameError::Oversize(_)));
}

#[tokio::test]
async fn empty_frame_is_rejected() {
    let (mut writer, mut reader) = tokio::io::duplex(64);
    writer.write_u32(0).await.unwrap();
    writer.flush().await.unwrap();
    let error = read_frame::<Sample, _>(&mut reader).await.unwrap_err();
    assert!(matches!(error, FrameError::Empty));
}
