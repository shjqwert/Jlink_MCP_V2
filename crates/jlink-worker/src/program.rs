use std::{fs, path::Path};

use jlink_domain::{
    ErrorCode, FirmwareImage, JlinkError, ProgramAfter, ProgramRequest, TargetConnectionSpec,
    TargetState, ValidationInvalidation, VerifyMismatchAccumulator, validate_flash_range,
    validate_image_flash_ranges,
};
use serde_json::json;

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
            after,
        } => {
            let image = read_image(&image, base_address)?;
            let regions = gateway.device_flash_regions(target.device())?;
            validate_image_flash_ranges(&image, &regions)?;
            if let Err(error) = gateway.program_image(&image) {
                gateway.close_target();
                session.record_execution_uncertain(ValidationInvalidation::FlashModified)?;
                return Err(error);
            }
            session.record_flash_modified();
            let verify_result = if verify {
                gateway
                    .reset_halt_for_flash()
                    .and_then(|()| verify_image(gateway, &image))
            } else {
                Ok(())
            };
            if let Err(error) = verify_result {
                let state = gateway
                    .observe_target_state()
                    .unwrap_or(TargetState::Unknown);
                session.record_program_state(state);
                return Err(error);
            }
            let state = apply_program_after(session, gateway, "flash", after)?;
            session.record_program_state(state);
            Ok(())
        }
        ProgramRequest::Erase { range, after } => {
            let regions = gateway.device_flash_regions(target.device())?;
            let result = if let Some(range) = range {
                validate_flash_range(&regions, range.address(), range.length())?;
                gateway.erase_range(range.address(), range.length())
            } else {
                gateway.erase_chip()
            };
            if let Err(error) = result {
                gateway.close_target();
                session.record_execution_uncertain(ValidationInvalidation::FlashModified)?;
                return Err(error);
            }
            session.record_flash_modified();
            let state = apply_program_after(session, gateway, "erase", after)?;
            session.record_program_state(state);
            Ok(())
        }
        ProgramRequest::Verify {
            image,
            base_address,
        } => {
            let image = read_image(&image, base_address)?;
            let regions = gateway.device_flash_regions(target.device())?;
            validate_image_flash_ranges(&image, &regions)?;
            verify_image(gateway, &image)?;
            let state = gateway.observe_target_state()?;
            session.record_program_state(state);
            Ok(())
        }
    }
}

fn apply_program_after(
    session: &mut TargetSessionManager,
    gateway: &mut DllGateway,
    operation: &'static str,
    after: ProgramAfter,
) -> Result<TargetState, JlinkError> {
    match gateway.apply_program_after(after) {
        Ok(state) => Ok(state),
        Err(cause) => {
            gateway.close_target();
            session.record_execution_uncertain(ValidationInvalidation::FlashModified)?;
            Err(post_action_uncertain_error(operation, after, &cause))
        }
    }
}

fn post_action_uncertain_error(
    operation: &'static str,
    after: ProgramAfter,
    cause: &JlinkError,
) -> JlinkError {
    JlinkError::new(
        ErrorCode::ExecutionUncertain,
        "Flash 主操作已成功，但后置状态处理失败；不得重放该 Flash 操作",
        false,
    )
    .with_detail("operation", json!(operation))
    .with_detail("phase", json!("post_action"))
    .with_detail("after", json!(after))
    .with_detail("flash_modified", json!(true))
    .with_detail("cause_code", json!(cause.code))
    .with_detail("cause_message", json!(cause.message))
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
        let cause = JlinkError::new(
            ErrorCode::TargetConnectFailed,
            "post-action ICSR read failed",
            true,
        );
        let error = post_action_uncertain_error("erase", ProgramAfter::ResetHalt, &cause);

        assert_eq!(error.code, ErrorCode::ExecutionUncertain);
        assert!(!error.retryable);
        let details = error.details.expect("phase details");
        assert_eq!(details["operation"], json!("erase"));
        assert_eq!(details["phase"], json!("post_action"));
        assert_eq!(details["after"], json!("reset_halt"));
        assert_eq!(details["flash_modified"], json!(true));
        assert_eq!(details["cause_code"], json!("TARGET_CONNECT_FAILED"));
    }
}
