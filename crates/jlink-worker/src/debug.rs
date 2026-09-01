use jlink_domain::{
    DebugRequest, DebugResult, DeviceMemoryMap, ErrorCode, FirmwareIdentityPlan,
    FirmwareIdentityStrength, JlinkError, MemoryRange, MemoryRegionKind, TargetConnectionSpec,
    WriteVerify, decode_typed_value, encode_typed_value, verify_memory_readback,
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
    match &request {
        DebugRequest::ReadVariable { firmware, .. } => {
            ensure_firmware_identity(session, gateway, target, firmware, false)?;
        }
        DebugRequest::WriteVariable { firmware, .. } => {
            ensure_firmware_identity(session, gateway, target, firmware, true)?;
        }
        DebugRequest::ReadMemory { .. }
        | DebugRequest::WriteMemory { .. }
        | DebugRequest::ReadRegister { .. }
        | DebugRequest::WriteRegister { .. } => {}
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

pub(crate) fn ensure_firmware_identity(
    session: &mut TargetSessionManager,
    gateway: &mut DllGateway,
    target: &TargetConnectionSpec,
    firmware: &FirmwareIdentityPlan,
    require_strong: bool,
) -> Result<FirmwareIdentityStrength, JlinkError> {
    firmware.validate()?;
    if firmware.strength() == FirmwareIdentityStrength::Weak {
        if require_strong {
            firmware.ensure_strong()?;
        }
        return Ok(FirmwareIdentityStrength::Weak);
    }
    if session.firmware_identity_cached(target, firmware) {
        return Ok(FirmwareIdentityStrength::Strong);
    }
    session.ensure_firmware_identity_read_allowed()?;
    let expected = firmware.ensure_strong()?;
    let length = expected.bytes().len();
    let range = MemoryRange::new(
        expected.address(),
        u64::try_from(length).map_err(|_| identity_unknown("固件身份读取长度无法表示", None))?,
    )?;
    let memory_map = gateway.device_memory_map(target.device())?;
    if memory_map.classify(range)? != MemoryRegionKind::Flash {
        return Err(identity_unknown(
            "固件身份块必须完整位于器件 Flash 区域",
            None,
        ));
    }
    let observed = gateway
        .read_bytes(expected.address(), length)
        .map_err(|error| identity_unknown("无法读取目标固定固件身份块", Some(&error)))?;
    firmware.verify_target_bytes(Some(&observed))?;
    session.record_firmware_identity(target, firmware);
    Ok(FirmwareIdentityStrength::Strong)
}

fn identity_unknown(message: &str, cause: Option<&JlinkError>) -> JlinkError {
    let error = JlinkError::new(ErrorCode::FirmwareIdentityUnknown, message, false);
    match cause {
        Some(cause) => error.with_detail("cause", serde_json::json!(cause.to_string())),
        None => error,
    }
}
