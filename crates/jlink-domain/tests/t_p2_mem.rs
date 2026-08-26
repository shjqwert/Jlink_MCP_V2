//! Primary T-P2-MEM verification for DBG-002, DBG-003, and DBG-006 rules.

use jlink_domain::{
    DeviceMemoryMap, ErrorCode, MemoryRange, MemoryReadOrigin, MemoryReadPlan, MemoryRegion,
    MemoryRegionKind, merge_safe_memory_reads, validate_write_count, verify_memory_readback,
};

fn device_map() -> DeviceMemoryMap {
    DeviceMemoryMap::new(vec![
        MemoryRegion::new(0x0000_0000, 0x0008_0000, MemoryRegionKind::Flash)
            .expect("program Flash"),
        MemoryRegion::new(0x1000_0000, 0x0001_0000, MemoryRegionKind::Flash).expect("data Flash"),
        MemoryRegion::new(0x1fff_8000, 0x0000_8000, MemoryRegionKind::Ram).expect("lower SRAM"),
        MemoryRegion::new(0x2000_0000, 0x0000_7000, MemoryRegionKind::Ram).expect("upper SRAM"),
    ])
    .expect("non-overlapping device map")
}

#[test]
fn t_p2_mem_classifies_ranges_and_rejects_raw_flash_writes() {
    let map = device_map();
    assert_eq!(
        map.classify(MemoryRange::raw(0x0001_0000, 16).expect("Flash read"))
            .expect("classified Flash"),
        MemoryRegionKind::Flash
    );
    assert_eq!(
        map.classify(MemoryRange::raw(0x2000_0010, 16).expect("RAM read"))
            .expect("classified RAM"),
        MemoryRegionKind::Ram
    );
    assert_eq!(
        map.classify(MemoryRange::raw(0x4000_1000, 4).expect("MMIO read"))
            .expect("classified MMIO"),
        MemoryRegionKind::Mmio
    );

    let flash = MemoryRange::raw(0x0001_0000, 4).expect("Flash range");
    let error = map
        .ensure_ordinary_write(flash)
        .expect_err("raw Flash write must use jlink_program");
    assert_eq!(error.code(), ErrorCode::AddressOutOfRange);
    let details = error.details.expect("replacement tool details");
    assert_eq!(details["region"], "flash");
    assert_eq!(details["use_tool"], "jlink_program");

    let crossing = MemoryRange::raw(0x1fff_fff0, 32).expect("valid address width");
    assert_eq!(
        map.classify(crossing)
            .expect_err("known RAM boundary crossing")
            .code(),
        ErrorCode::AddressOutOfRange
    );
    assert_eq!(
        MemoryRange::raw(0, 4_097)
            .expect_err("raw transfer limit")
            .code(),
        ErrorCode::ValueInvalid
    );
    assert_eq!(
        MemoryRange::raw(u32::MAX.into(), 2)
            .expect_err("Cortex-M address overflow")
            .code(),
        ErrorCode::AddressOutOfRange
    );
}

#[test]
fn t_p2_mem_merges_only_aligned_side_effect_free_adjacent_reads() {
    let safe = [
        MemoryReadPlan::new(
            MemoryRange::new(0x2000_0000, 4).expect("first range"),
            MemoryRegionKind::Ram,
            MemoryReadOrigin::Raw,
            false,
        ),
        MemoryReadPlan::new(
            MemoryRange::new(0x2000_0004, 8).expect("second range"),
            MemoryRegionKind::Ram,
            MemoryReadOrigin::StaticVariable,
            false,
        ),
    ];
    let merged = merge_safe_memory_reads(&safe);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].range.address(), 0x2000_0000);
    assert_eq!(merged[0].range.length(), 12);
    assert_eq!(merged[0].parts[1].offset, 4);

    let mmio = [
        MemoryReadPlan::new(
            MemoryRange::new(0x4000_0000, 4).expect("first MMIO"),
            MemoryRegionKind::Mmio,
            MemoryReadOrigin::StaticVariable,
            false,
        ),
        MemoryReadPlan::new(
            MemoryRange::new(0x4000_0004, 4).expect("second MMIO"),
            MemoryRegionKind::Mmio,
            MemoryReadOrigin::StaticVariable,
            false,
        ),
    ];
    assert_eq!(merge_safe_memory_reads(&mmio).len(), 2);

    let volatile = [
        safe[0],
        MemoryReadPlan::new(
            MemoryRange::new(0x2000_0004, 4).expect("adjacent volatile RAM"),
            MemoryRegionKind::Ram,
            MemoryReadOrigin::StaticVariable,
            true,
        ),
    ];
    assert_eq!(merge_safe_memory_reads(&volatile).len(), 2);

    let unaligned = [
        MemoryReadPlan::new(
            MemoryRange::new(0x2000_0002, 2).expect("unaligned first RAM range"),
            MemoryRegionKind::Ram,
            MemoryReadOrigin::Raw,
            false,
        ),
        MemoryReadPlan::new(
            MemoryRange::new(0x2000_0004, 4).expect("adjacent aligned RAM range"),
            MemoryRegionKind::Ram,
            MemoryReadOrigin::Raw,
            false,
        ),
    ];
    assert_eq!(merge_safe_memory_reads(&unaligned).len(), 2);

    let non_adjacent = [
        safe[0],
        MemoryReadPlan::new(
            MemoryRange::new(0x2000_0008, 4).expect("non-adjacent RAM"),
            MemoryRegionKind::Ram,
            MemoryReadOrigin::Raw,
            false,
        ),
    ];
    assert_eq!(merge_safe_memory_reads(&non_adjacent).len(), 2);

    let cross_region = [
        safe[0],
        MemoryReadPlan::new(
            MemoryRange::new(0x2000_0004, 4).expect("same address shape"),
            MemoryRegionKind::Flash,
            MemoryReadOrigin::StaticVariable,
            false,
        ),
    ];
    assert_eq!(merge_safe_memory_reads(&cross_region).len(), 2);
}

#[test]
fn t_p2_mem_reports_short_writes_and_first_readback_difference() {
    validate_write_count(0x2000_1000, 4, 4).expect("complete write");
    let short = validate_write_count(0x2000_1000, 4, 2).expect_err("short write");
    assert_eq!(short.code(), ErrorCode::ExecutionUncertain);
    let details = short.details.expect("short-write details");
    assert_eq!(details["requested_length"], 4);
    assert_eq!(details["actual_length"], 2);

    verify_memory_readback(0x2000_1000, &[1, 2, 3, 4], &[1, 2, 3, 4]).expect("matching readback");
    let mismatch = verify_memory_readback(0x2000_1000, &[1, 2, 3, 4], &[1, 9, 3, 0])
        .expect_err("readback mismatch");
    assert_eq!(mismatch.code(), ErrorCode::VerifyFailed);
    let details = mismatch.details.expect("first-difference details");
    assert_eq!(details.len(), 3);
    assert_eq!(details["first_address"], "0x20001001");
    assert_eq!(details["expected"], "02");
    assert_eq!(details["actual"], "09");
}
