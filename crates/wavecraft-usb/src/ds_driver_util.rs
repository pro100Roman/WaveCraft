//! Helpers shared by the DSLogic and DSCope capture drivers.

use crate::UsbError;
use wavecraft_core::CaptureError;

pub(crate) fn capture_error(error: UsbError) -> CaptureError {
    CaptureError::new(error.to_string())
}

/// Combines the result of a capture with the result of cleaning up after it,
/// keeping both messages when both failed.
pub(crate) fn combine_results(
    primary: Result<(), CaptureError>,
    cleanup: Result<(), CaptureError>,
    cleanup_action: &str,
) -> Result<(), CaptureError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(CaptureError::new(format!(
            "{primary}; additionally failed to {cleanup_action}: {cleanup}"
        ))),
    }
}

pub(crate) fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

/// Reads a little-endian `u16`. The caller must have checked that
/// `offset + 2 <= bytes.len()`; every device-facing caller validates the
/// transfer length first.
pub(crate) fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

/// Reads a little-endian `u32`. The caller must have checked that
/// `offset + 4 <= bytes.len()`; every device-facing caller validates the
/// transfer length first.
pub(crate) fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_round_trip_as_little_endian() {
        let mut bytes = Vec::new();
        push_u16(&mut bytes, 0x1234);
        push_u32(&mut bytes, 0xa1b2_c3d4);

        assert_eq!(bytes, [0x34, 0x12, 0xd4, 0xc3, 0xb2, 0xa1]);
        assert_eq!(read_u16(&bytes, 0), 0x1234);
        assert_eq!(read_u32(&bytes, 2), 0xa1b2_c3d4);
    }

    #[test]
    fn combined_results_report_both_failures() {
        assert!(combine_results(Ok(()), Ok(()), "stop").is_ok());
        assert_eq!(
            combine_results(Err(CaptureError::new("capture")), Ok(()), "stop")
                .unwrap_err()
                .to_string(),
            "capture"
        );
        assert_eq!(
            combine_results(Ok(()), Err(CaptureError::new("cleanup")), "stop")
                .unwrap_err()
                .to_string(),
            "cleanup"
        );
        assert_eq!(
            combine_results(
                Err(CaptureError::new("capture")),
                Err(CaptureError::new("cleanup")),
                "stop"
            )
            .unwrap_err()
            .to_string(),
            "capture; additionally failed to stop: cleanup"
        );
    }
}
