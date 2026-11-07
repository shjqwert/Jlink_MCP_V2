use std::{fs, path::Path};

use jlink_domain::{
    ErrorCode, FirmwareIdentityPlan, FirmwareImage, FlashModifiedState, JlinkError,
    ProgramExecutionFacts, ProgramRequest, ProgramStage, TargetConnectionSpec, TargetState,
    ValidationInvalidation, VerifyMismatchAccumulator, validate_flash_range,
    validate_image_flash_ranges,
};

use crate::{gateway::DllGateway, session::TargetSessionManager};

/// Executes one already Schema-validated Flash request on the unique gateway.
///
/// HSS and active-session conflicts are rejected before file or DLL access.
/// Image parsing and all device-region checks complete before the first target
/// side effect. A successful Flash mutation invalidates session validation.
pub(crate) fn execute_program(
    session: &mut TargetSessionManager,
    gateway: &mut DllGateway,
    target: &TargetConnectionSpec,
    request: ProgramRequest,
) -> Result<(), JlinkError> {
    session.ensure_program_allowed(target)?;
    match request {
        ProgramRequest::Flash {
            image,
            base_address,
            verify,
            symbols_elf,
            after,
            loader_ram,
        } => {
            let loader_ram = loader_ram.ok_or_else(missing_loader_ram_error)?;
            let image = read_image(&image, base_address)?;
            let regions = gateway.device_flash_regions(target.device())?;
            validate_image_flash_ranges(&image, &regions)?;
            let firmware = verify
                .then(|| verified_symbol_plan(symbols_elf.as_deref(), &image))
                .flatten();
            let mut facts = ProgramExecutionFacts::new();
            if let Err(error) = gateway.program_image(&image, loader_ram, &mut facts) {
                return handle_program_failure(session, gateway, facts, error);
            }
            session.record_flash_modified();
            if verify {
                if let Err(error) = gateway.reset_halt_for_flash() {
                    return handle_program_failure(session, gateway, facts, error);
                }
                facts.last_trusted_target_state = Some(TargetState::Halted);
                facts.advance(
                    ProgramStage::VerifyPreparation,
                    ProgramStage::RangeVerification,
                );
                if let Err(error) = verify_image(gateway, &image) {
                    let state = gateway
                        .observe_target_state()
                        .unwrap_or(TargetState::Unknown);
                    facts.last_trusted_target_state = Some(state);
                    session.record_program_state(state);
                    return Err(facts.known_error(error));
                }
                facts.advance(ProgramStage::RangeVerification, ProgramStage::FinalState);
            } else {
                facts.current_stage = ProgramStage::FinalState;
            }
            let state = match gateway.apply_program_after(after) {
                Ok(state) => state,
                Err(error) => return handle_program_failure(session, gateway, facts, error),
            };
            facts.last_completed_stage = Some(ProgramStage::FinalState);
            facts.last_trusted_target_state = Some(state);
            session.record_program_state(state);
            if let Some(firmware) = firmware {
                session.record_firmware_identity(target, &firmware);
            }
            Ok(())
        }
        ProgramRequest::Erase {
            range,
            after,
            loader_ram,
        } => {
            let loader_ram = loader_ram.ok_or_else(missing_loader_ram_error)?;
            let regions = gateway.device_flash_regions(target.device())?;
            let mut facts = ProgramExecutionFacts::new();
            let result = if let Some(range) = range {
                validate_flash_range(&regions, range.address(), range.length())?;
                gateway.erase_range(range.address(), range.length(), loader_ram, &mut facts)
            } else {
                gateway.erase_chip(loader_ram, &mut facts)
            };
            if let Err(error) = result {
                return handle_program_failure(session, gateway, facts, error);
            }
            session.record_flash_modified();
            let state = match gateway.apply_program_after(after) {
                Ok(state) => state,
                Err(error) => return handle_program_failure(session, gateway, facts, error),
            };
            facts.last_completed_stage = Some(ProgramStage::FinalState);
            facts.last_trusted_target_state = Some(state);
            session.record_program_state(state);
            Ok(())
        }
        ProgramRequest::Verify {
            image,
            base_address,
            symbols_elf,
        } => {
            let image = read_image(&image, base_address)?;
            let regions = gateway.device_flash_regions(target.device())?;
            validate_image_flash_ranges(&image, &regions)?;
            verify_program_request(session, gateway, target, &image, symbols_elf.as_deref())
        }
    }
}

fn verify_program_request(
    session: &mut TargetSessionManager,
    gateway: &mut DllGateway,
    target: &TargetConnectionSpec,
    image: &FirmwareImage,
    symbols_elf: Option<&Path>,
) -> Result<(), JlinkError> {
    let firmware = verified_symbol_plan(symbols_elf, image);
    session.clear_firmware_identity();
    let mut facts = ProgramExecutionFacts::new();
    facts.current_stage = ProgramStage::RangeVerification;
    if let Err(error) = verify_image(gateway, image) {
        let state = gateway
            .observe_target_state()
            .unwrap_or(TargetState::Unknown);
        facts.last_trusted_target_state = Some(state);
        session.record_program_state(state);
        return Err(facts.known_error(error));
    }
    session.record_program_state(gateway.observe_target_state()?);
    if let Some(firmware) = firmware {
        session.record_firmware_identity(target, &firmware);
    }
    Ok(())
}

fn verified_symbol_plan(
    path: Option<&Path>,
    image: &FirmwareImage,
) -> Option<FirmwareIdentityPlan> {
    let elf = read_image(path?, None).ok()?;
    // The caller already validated every byte of image against the device Flash map.
    if !elf.is_covered_by(image) {
        return None;
    }
    elf.symbol_identity_plan().ok()
}

fn handle_program_failure(
    session: &mut TargetSessionManager,
    gateway: &mut DllGateway,
    mut facts: ProgramExecutionFacts,
    error: JlinkError,
) -> Result<(), JlinkError> {
    if facts.last_trusted_target_state.is_none() {
        facts.last_trusted_target_state = gateway.observe_target_state().ok();
    }
    if !facts.side_effect_dispatched {
        return Err(facts.known_error(error));
    }
    gateway.close_target();
    let invalidation = match facts.flash_modified {
        FlashModifiedState::True | FlashModifiedState::Unknown => {
            ValidationInvalidation::FlashModified
        }
        FlashModifiedState::False => ValidationInvalidation::TargetConfigurationChanged,
    };
    session.record_execution_uncertain(invalidation)?;
    Err(facts.uncertain_error(&error))
}

fn missing_loader_ram_error() -> JlinkError {
    JlinkError::new(
        ErrorCode::ConfigInvalid,
        "Flash programming requires profile.loader_ram for the no-Flash preflight",
        false,
    )
    .with_detail("field", serde_json::json!("profile.loader_ram"))
}

fn read_image(path: &Path, base_address: Option<u64>) -> Result<FirmwareImage, JlinkError> {
    let bytes = fs::read(path).map_err(|error| {
        JlinkError::new(
            ErrorCode::ValueInvalid,
            format!("无法读取固件镜像 {}：{error}", path.display()),
            false,
        )
    })?;
    FirmwareImage::parse(&path.to_string_lossy(), &bytes, base_address)
}

fn verify_image(gateway: &mut DllGateway, image: &FirmwareImage) -> Result<(), JlinkError> {
    let mut mismatches = VerifyMismatchAccumulator::new();
    for segment in image.segments() {
        let actual = gateway.read_bytes(segment.address(), segment.data().len())?;
        mismatches.compare_segment(segment.address(), segment.data(), &actual)?;
    }
    match mismatches.finish() {
        Some(mismatch) => Err(mismatch.into_error()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_action_failure_is_non_retryable_and_preserves_phase_facts() {
        let mut facts = ProgramExecutionFacts::new();
        facts.side_effect_dispatched = true;
        facts.flash_modified = FlashModifiedState::True;
        facts.last_completed_stage = Some(ProgramStage::RangeVerification);
        facts.current_stage = ProgramStage::FinalState;
        let cause = JlinkError::new(
            ErrorCode::TargetConnectFailed,
            "post-action ICSR read failed",
            true,
        );
        let error = facts.uncertain_error(&cause);

        assert_eq!(error.code, ErrorCode::ExecutionUncertain);
        assert!(!error.retryable);
        let details = error.details.expect("phase details");
        assert_eq!(
            details["last_completed_stage"],
            serde_json::json!("range_verification")
        );
        assert_eq!(details["current_stage"], serde_json::json!("final_state"));
        assert_eq!(details["flash_modified"], serde_json::json!(true));
        assert_eq!(
            details["cause_code"],
            serde_json::json!("TARGET_CONNECT_FAILED")
        );
    }
}
