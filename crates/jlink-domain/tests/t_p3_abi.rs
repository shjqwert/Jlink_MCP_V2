//! Primary T-P3-ABI assertions for the frozen raw HSS frame contract.

use jlink_domain::{
    ErrorCode, HSS_BLOCK_FLAGS_DEFAULT, HSS_START_FLAG_TIMESTAMP_US_EXPERIMENTAL,
    HSS_START_FLAGS_698A_MAINLINE, HssFrameLayout,
};

#[test]
fn t_p3_abi_parses_frozen_little_endian_frames_and_preserves_tail() {
    assert_eq!(HSS_BLOCK_FLAGS_DEFAULT, 0);
    assert_eq!(HSS_START_FLAGS_698A_MAINLINE, 0);
    assert_eq!(HSS_START_FLAG_TIMESTAMP_US_EXPERIMENTAL, 1);

    let layout = HssFrameLayout::new(&[4; 10]).expect("F0-A 10x32-bit layout");
    assert_eq!(layout.sample_bytes(), 40);
    assert_eq!(layout.record_bytes(), 44);

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&7_u32.to_le_bytes());
    bytes.extend(0_u8..40);
    bytes.extend_from_slice(&8_u32.to_le_bytes());
    bytes.extend(40_u8..80);
    bytes.extend_from_slice(&[0xAA, 0xBB]);

    let batch = layout.parse(&bytes).expect("parse complete frozen frames");
    assert_eq!(batch.frames.len(), 2);
    assert_eq!(batch.frames[0].timestamp_raw, 7);
    assert_eq!(batch.frames[0].sample, &(0_u8..40).collect::<Vec<_>>());
    assert_eq!(batch.frames[1].timestamp_raw, 8);
    assert_eq!(batch.frames[1].sample, &(40_u8..80).collect::<Vec<_>>());
    assert_eq!(batch.incomplete_tail, [0xAA, 0xBB]);
}

#[test]
fn t_p3_abi_rejects_invalid_frame_layout_without_truncation() {
    for counts in [&[][..], &[0][..], &[u32::MAX, 1][..]] {
        let error = HssFrameLayout::new(counts).expect_err("invalid HSS frame layout");
        assert_eq!(error.code(), ErrorCode::FrameInvalid);
    }
}
