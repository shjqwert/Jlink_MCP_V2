use jlink_domain::{
    DebugRequest, DebugResult, DeviceMemoryMap, ErrorCode, FirmwareIdentityPlan,
    FirmwareSegmentFingerprint, JlinkError, MemoryRange, WriteVerify, decode_typed_value,
    encode_typed_value, verify_memory_readback,
};

use crate::{gateway::DllGateway, session::TargetSessionManager};

/// Executes one prevalidated ordinary memory or typed-variable request.
pub(crate) fn execute_debug(
    session: &mut TargetSessionManager,
    gateway: &mut DllGateway,
    target: &jlink_domain::TargetConnectionSpec,
    request: DebugRequest,
) -> Result<DebugResult, JlinkError> {
    session.ensure_debug_allowed(target, &request)?;
    if let Some(firmware) = request.firmware() {
        ensure_firmware_identity(session, gateway, firmware)?;
    }
    match request {
        DebugRequest::ReadMemory { range } => {
            let memory_map = gateway.device_memory_map(target.device())?;
            read_memory(gateway, &memory_map, range)
        }
        DebugRequest::WriteMemory {
            address,
            data,
            verify,
        } => {
            let memory_map = gateway.device_memory_map(target.device())?;
            write_memory(gateway, &memory_map, address, &data, verify)
        }
        DebugRequest::ReadVariable { plan, .. } => {
            let memory_map = gateway.device_memory_map(target.device())?;
            let range = MemoryRange::new(plan.address(), plan.byte_size())?;
            memory_map.classify(range)?;
            let length = usize::try_from(plan.byte_size()).map_err(|_| {
                JlinkError::new(ErrorCode::ValueInvalid, "变量读取长度无法表示", false)
            })?;
            let data = gateway.read_bytes(plan.address(), length)?;
            let value = decode_typed_value(&plan, &data)?;
            Ok(DebugResult::Variable { value })
        }
        DebugRequest::WriteVariable {
            plan,
            value,
            verify,
            ..
        } => {
            let memory_map = gateway.device_memory_map(target.device())?;
            let range = MemoryRange::new(plan.address(), plan.byte_size())?;
            memory_map.ensure_ordinary_write(range)?;
            let length = usize::try_from(plan.byte_size()).map_err(|_| {
                JlinkError::new(ErrorCode::ValueInvalid, "变量写入长度无法表示", false)
            })?;
            let current = gateway.read_bytes(plan.address(), length)?;
            let encoded = encode_typed_value(&plan, &current, &value)?;
            gateway.write_bytes(plan.address(), &encoded)?;
            verify_if_requested(gateway, plan.address(), &encoded, verify)?;
            Ok(DebugResult::Written)
        }
        DebugRequest::ReadRegister { register } => {
            let value = gateway.read_register(register)?;
            Ok(DebugResult::Register { value })
        }
        DebugRequest::WriteRegister { register, value } => {
            gateway.write_register(register, value)?;
            Ok(DebugResult::Written)
        }
    }
}

fn read_memory(
    gateway: &mut DllGateway,
    memory_map: &DeviceMemoryMap,
    range: MemoryRange,
) -> Result<DebugResult, JlinkError> {
    memory_map.classify(range)?;
    let length = usize::try_from(range.length())
        .map_err(|_| JlinkError::new(ErrorCode::ValueInvalid, "内存读取长度无法表示", false))?;
    let data = gateway.read_bytes(range.address(), length)?;
    Ok(DebugResult::Memory { data })
}

fn write_memory(
    gateway: &mut DllGateway,
    memory_map: &DeviceMemoryMap,
    address: u64,
    data: &[u8],
    verify: WriteVerify,
) -> Result<DebugResult, JlinkError> {
    let length = u64::try_from(data.len())
        .map_err(|_| JlinkError::new(ErrorCode::ValueInvalid, "内存写入长度无法表示", false))?;
    let range = MemoryRange::raw(address, length)?;
    memory_map.ensure_ordinary_write(range)?;
    gateway.write_bytes(address, data)?;
    verify_if_requested(gateway, address, data, verify)?;
    Ok(DebugResult::Written)
}

fn verify_if_requested(
    gateway: &mut DllGateway,
    address: u64,
    expected: &[u8],
    verify: WriteVerify,
) -> Result<(), JlinkError> {
    if verify == WriteVerify::Readback {
        let actual = gateway.read_bytes(address, expected.len())?;
        verify_memory_readback(address, expected, &actual)?;
    }
    Ok(())
}

fn ensure_firmware_identity(
    session: &mut TargetSessionManager,
    gateway: &mut DllGateway,
    firmware: &FirmwareIdentityPlan,
) -> Result<(), JlinkError> {
    firmware.validate()?;
    if session.firmware_identity_cached(firmware.elf_sha256()) {
        return Ok(());
    }
    session.ensure_firmware_identity_read_allowed()?;
    let observed = firmware
        .segments()
        .iter()
        .map(|segment| {
            let length = usize::try_from(segment.length())
                .map_err(|_| identity_unknown("固件身份读取长度无法表示", None))?;
            let data = gateway
                .read_bytes(segment.address(), length)
                .map_err(|error| {
                    identity_unknown("无法完整读取目标 Flash 以验证符号 ELF", Some(&error))
                })?;
            FirmwareSegmentFingerprint::from_bytes(segment.address(), &data)
                .map_err(|error| identity_unknown("固件身份读取证据无效", Some(&error)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    firmware.verify_target(Some(&observed))?;
    session.record_firmware_identity(firmware.elf_sha256());
    Ok(())
}

fn identity_unknown(message: &str, cause: Option<&JlinkError>) -> JlinkError {
    let error = JlinkError::new(ErrorCode::FirmwareIdentityUnknown, message, false);
    match cause {
        Some(cause) => error.with_detail("cause", serde_json::json!(cause.to_string())),
        None => error,
    }
}
