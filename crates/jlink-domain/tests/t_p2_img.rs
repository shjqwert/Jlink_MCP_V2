//! Primary test T-P2-IMG for ART-001 and ART-002.

use jlink_domain::{
    ErrorCode, FirmwareFormat, FirmwareIdentityPlan, FirmwareImage, FirmwareSegmentFingerprint,
};
use serde_json::json;

#[test]
fn t_p2_img_accepts_supported_formats_and_enforces_bin_base_address() {
    let elf = minimal_dwarf_elf(0x0001_0910, &[1, 2, 3, 4]);
    for file_name in ["fixture.elf", "fixture.axf", "fixture.out"] {
        let image = FirmwareImage::parse(file_name, &elf, None).expect("valid symbol ELF");
        assert_eq!(image.format(), FirmwareFormat::Elf);
        assert_eq!(image.sha256().len(), 64);
        assert_eq!(image.segments().len(), 1);
        assert_eq!(image.segments()[0].address(), 0x0001_0910);
        assert_eq!(image.segments()[0].data(), [1, 2, 3, 4]);
        image
            .symbol_identity_plan()
            .expect("ELF contains DWARF information");
    }

    let hex = b":0400100001020304E2\n:00000001FF\n";
    let image = FirmwareImage::parse("fixture.hex", hex, None).expect("valid Intel HEX");
    assert_eq!(image.format(), FirmwareFormat::IntelHex);
    assert_eq!(image.segments()[0].address(), 0x10);
    assert_eq!(image.segments()[0].data(), [1, 2, 3, 4]);

    let s_record = b"S107001001020304DE\nS5030001FB\nS9030000FC\n";
    let image = FirmwareImage::parse("fixture.srec", s_record, None).expect("valid S-record");
    assert_eq!(image.format(), FirmwareFormat::SRecord);
    assert_eq!(image.segments()[0].address(), 0x10);
    assert_eq!(image.segments()[0].data(), [1, 2, 3, 4]);

    let missing = FirmwareImage::parse("fixture.bin", &[1, 2, 3, 4], None)
        .expect_err("BIN base address is mandatory");
    assert_eq!(missing.code, ErrorCode::ValueInvalid);
    let bin = FirmwareImage::parse("fixture.bin", &[1, 2, 3, 4], Some(0x2000))
        .expect("BIN with explicit base address");
    assert_eq!(bin.format(), FirmwareFormat::Bin);
    assert_eq!(bin.segments()[0].address(), 0x2000);

    let redundant = FirmwareImage::parse("fixture.hex", hex, Some(0x1000))
        .expect_err("self-addressed formats reject base_address");
    assert_eq!(redundant.code, ErrorCode::ValueInvalid);
    let map =
        FirmwareImage::parse("fixture.map", &elf, None).expect_err("MAP is never a symbol source");
    assert_eq!(map.code, ErrorCode::ValueInvalid);
}

#[test]
fn t_p2_img_rejects_corrupt_or_ambiguous_image_records() {
    let bad_hex = FirmwareImage::parse("fixture.hex", b":0400100001020304E3\n:00000001FF\n", None)
        .expect_err("invalid Intel HEX checksum");
    assert_eq!(bad_hex.code, ErrorCode::ValueInvalid);

    let bad_s_record =
        FirmwareImage::parse("fixture.srec", b"S107001001020304DF\nS9030000FC\n", None)
            .expect_err("invalid S-record checksum");
    assert_eq!(bad_s_record.code, ErrorCode::ValueInvalid);
    let bad_s_record_count = FirmwareImage::parse(
        "fixture.srec",
        b"S107001001020304DE\nS5030002FA\nS9030000FC\n",
        None,
    )
    .expect_err("S5 count must match the number of data records");
    assert_eq!(bad_s_record_count.code, ErrorCode::ValueInvalid);
    let data_after_count = FirmwareImage::parse(
        "fixture.srec",
        b"S107001001020304DE\nS5030001FB\nS1040020AA31\nS9030000FC\n",
        None,
    )
    .expect_err("S5 count record closes the data-record sequence");
    assert_eq!(data_after_count.code, ErrorCode::ValueInvalid);

    let no_dwarf = minimal_elf(0x1000, &[1, 2, 3, 4], false);
    let error = FirmwareImage::parse("fixture.out", &no_dwarf, None)
        .expect("valid ELF image")
        .symbol_identity_plan()
        .expect_err("symbol ELF requires DWARF");
    assert_eq!(error.code, ErrorCode::ValueInvalid);
}

#[test]
fn t_p2_img_identity_distinguishes_unknown_mismatch_and_match() {
    let elf = minimal_dwarf_elf(0x1000, &[1, 2, 3, 4]);
    let plan = FirmwareImage::parse("fixture.out", &elf, None)
        .expect("valid IAR-style OUT content")
        .symbol_identity_plan()
        .expect("symbol identity plan");
    assert_eq!(plan.elf_sha256().len(), 64);
    assert_eq!(plan.segments().len(), 1);

    let unknown = plan
        .verify_target(None)
        .expect_err("missing readback cannot prove identity");
    assert_eq!(unknown.code, ErrorCode::FirmwareIdentityUnknown);
    let incomplete = plan
        .verify_target(Some(&[]))
        .expect_err("empty readback cannot prove identity");
    assert_eq!(incomplete.code, ErrorCode::FirmwareIdentityUnknown);

    let empty_plan: FirmwareIdentityPlan = serde_json::from_value(json!({
        "elf_sha256": "00",
        "segments": []
    }))
    .expect("wire shape remains deserializable for validation");
    let invalid = empty_plan
        .verify_target(Some(&[]))
        .expect_err("empty identity plan must not succeed vacuously");
    assert_eq!(invalid.code, ErrorCode::FirmwareIdentityUnknown);

    let expected = FirmwareSegmentFingerprint::from_bytes(0x1000, &[1, 2, 3, 4])
        .expect("expected readback fingerprint");
    plan.verify_target(Some(std::slice::from_ref(&expected)))
        .expect("matching target readback");

    let actual = FirmwareSegmentFingerprint::from_bytes(0x1000, &[1, 2, 3, 5])
        .expect("different readback fingerprint");
    let mismatch = plan
        .verify_target(Some(&[actual]))
        .expect_err("different target bytes prove mismatch");
    assert_eq!(mismatch.code, ErrorCode::FirmwareElfMismatch);
    assert_eq!(
        mismatch.details.as_ref().expect("mismatch details")["address"],
        "0x1000"
    );
}

fn minimal_dwarf_elf(load_address: u32, payload: &[u8]) -> Vec<u8> {
    minimal_elf(load_address, payload, true)
}

fn minimal_elf(load_address: u32, payload: &[u8], with_dwarf: bool) -> Vec<u8> {
    const ELF_HEADER_SIZE: usize = 52;
    const PROGRAM_HEADER_OFFSET: usize = ELF_HEADER_SIZE;
    const LOAD_OFFSET: usize = 0x100;
    const DEBUG_OFFSET: usize = 0x110;
    const STRING_OFFSET: usize = 0x120;
    const SECTION_OFFSET: usize = 0x140;
    const SECTION_HEADER_SIZE: usize = 40;
    const SECTION_COUNT: usize = 3;
    const STRINGS: &[u8] = b"\0.shstrtab\0.debug_info\0";

    assert!(payload.len() <= DEBUG_OFFSET - LOAD_OFFSET);
    let mut elf = vec![0_u8; SECTION_OFFSET + SECTION_HEADER_SIZE * SECTION_COUNT];
    elf[0..16].copy_from_slice(&[0x7f, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    put_u16(&mut elf, 16, 2);
    put_u16(&mut elf, 18, 40);
    put_u32(&mut elf, 20, 1);
    put_u32(&mut elf, 24, load_address);
    put_u32(
        &mut elf,
        28,
        u32::try_from(PROGRAM_HEADER_OFFSET).expect("program offset"),
    );
    put_u32(
        &mut elf,
        32,
        u32::try_from(SECTION_OFFSET).expect("section offset"),
    );
    put_u32(&mut elf, 36, 0x0500_0000);
    put_u16(
        &mut elf,
        40,
        u16::try_from(ELF_HEADER_SIZE).expect("ELF header size"),
    );
    put_u16(&mut elf, 42, 32);
    put_u16(&mut elf, 44, 1);
    put_u16(
        &mut elf,
        46,
        u16::try_from(SECTION_HEADER_SIZE).expect("section header size"),
    );
    put_u16(
        &mut elf,
        48,
        u16::try_from(SECTION_COUNT).expect("section count"),
    );
    put_u16(&mut elf, 50, 1);

    put_u32(&mut elf, PROGRAM_HEADER_OFFSET, 1);
    put_u32(
        &mut elf,
        PROGRAM_HEADER_OFFSET + 4,
        u32::try_from(LOAD_OFFSET).expect("load offset"),
    );
    put_u32(&mut elf, PROGRAM_HEADER_OFFSET + 8, 0x2000_0000);
    put_u32(&mut elf, PROGRAM_HEADER_OFFSET + 12, load_address);
    put_u32(
        &mut elf,
        PROGRAM_HEADER_OFFSET + 16,
        u32::try_from(payload.len()).expect("payload length"),
    );
    put_u32(
        &mut elf,
        PROGRAM_HEADER_OFFSET + 20,
        u32::try_from(payload.len()).expect("payload length"),
    );
    put_u32(&mut elf, PROGRAM_HEADER_OFFSET + 24, 4);
    put_u32(&mut elf, PROGRAM_HEADER_OFFSET + 28, 4);
    elf[LOAD_OFFSET..LOAD_OFFSET + payload.len()].copy_from_slice(payload);
    elf[STRING_OFFSET..STRING_OFFSET + STRINGS.len()].copy_from_slice(STRINGS);

    let string_section = SECTION_OFFSET + SECTION_HEADER_SIZE;
    put_u32(&mut elf, string_section, 1);
    put_u32(&mut elf, string_section + 4, 3);
    put_u32(
        &mut elf,
        string_section + 16,
        u32::try_from(STRING_OFFSET).expect("string offset"),
    );
    put_u32(
        &mut elf,
        string_section + 20,
        u32::try_from(STRINGS.len()).expect("string length"),
    );
    put_u32(&mut elf, string_section + 32, 1);

    let debug_section = SECTION_OFFSET + SECTION_HEADER_SIZE * 2;
    put_u32(&mut elf, debug_section, 11);
    put_u32(&mut elf, debug_section + 4, 1);
    put_u32(
        &mut elf,
        debug_section + 16,
        u32::try_from(DEBUG_OFFSET).expect("debug offset"),
    );
    if with_dwarf {
        elf[DEBUG_OFFSET..DEBUG_OFFSET + 4].copy_from_slice(&[1, 2, 3, 4]);
        put_u32(&mut elf, debug_section + 20, 4);
    }
    put_u32(&mut elf, debug_section + 32, 1);
    elf
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
