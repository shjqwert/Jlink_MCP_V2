use std::{fs, path::Path};

use jlink_domain::{
    ErrorCode, FirmwareImage, JlinkError, ProgramRequest, TargetConnectionSpec, TargetState,
    VerifyMismatchAccumulator, validate_flash_range, validate_image_flash_ranges,
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
            after,
        } => {
            let image = read_image(&image, base_address)?;
            let regions = gateway.device_flash_regions(target.device())?;
            validate_image_flash_ranges(&image, &regions)?;
            if let Err(error) = gateway.program_image(&image) {
                gateway.close_target();
                session.record_program_uncertain()?;
                return Err(error);
            }
            if verify && let Err(error) = verify_image(gateway, &image) {
                let state = gateway
                    .observe_target_state()
                    .unwrap_or(TargetState::Unknown);
                session.record_program_result(state, true);
                return Err(error);
            }
            let state = gateway.apply_program_after(after)?;
            session.record_program_result(state, true);
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
                session.record_program_uncertain()?;
                return Err(error);
            }
            let state = gateway.apply_program_after(after)?;
            session.record_program_result(state, true);
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
            session.record_program_result(state, false);
            Ok(())
        }
    }
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
