#![cfg_attr(target_os = "espidf", no_std)]

use core::slice;

pub mod aggregation;

#[cfg(feature = "cardputer-ui")]
pub mod audio;
pub mod feedback;
pub mod protocol;
#[cfg(target_os = "espidf")]
mod runtime;
pub mod state;
#[cfg(feature = "cardputer-ui")]
pub mod ui;
#[cfg(feature = "cardputer-ui")]
mod ui_brand;
#[cfg(feature = "cardputer-ui")]
mod ui_fonts;
#[cfg(feature = "cardputer-ui")]
mod ui_orb;
#[cfg(feature = "cardputer-ui")]
mod ui_strings;
pub mod uplink;

#[cfg(target_os = "espidf")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    extern "C" {
        fn abort() -> !;
    }
    // ESP-IDF's panic handler resets the device instead of leaving a task spinning forever.
    unsafe { abort() }
}

use omdrop_proto::{cobs_decode_in_place, cobs_encode, crc16_ccitt_false};

/// CRC entry point for the ESP-IDF C transport.
///
/// # Safety
/// `data` must reference `len` readable bytes, or be null when `len` is zero.
#[no_mangle]
pub unsafe extern "C" fn owl_proto_crc16(data: *const u8, len: usize) -> u16 {
    if len == 0 {
        return crc16_ccitt_false(&[]);
    }
    if data.is_null() {
        return 0;
    }
    crc16_ccitt_false(slice::from_raw_parts(data, len))
}

/// COBS encoder entry point. Returns the encoded length, or zero on invalid pointers/capacity.
///
/// # Safety
/// Input and output must reference their declared lengths and must not overlap.
#[no_mangle]
pub unsafe extern "C" fn owl_proto_cobs_encode(
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_cap: usize,
) -> usize {
    if (input_len != 0 && input.is_null())
        || output.is_null()
        || output_cap < input_len.saturating_add(input_len / 254 + 2)
    {
        return 0;
    }
    let input = if input_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(input, input_len)
    };
    cobs_encode(input, slice::from_raw_parts_mut(output, output_cap)).unwrap_or(0)
}

/// In-place COBS decoder entry point. Returns the decoded length, or -1 on malformed input.
///
/// # Safety
/// `buffer` must reference `len` exclusively writable bytes.
#[no_mangle]
pub unsafe extern "C" fn owl_proto_cobs_decode_in_place(buffer: *mut u8, len: usize) -> isize {
    if buffer.is_null() {
        return -1;
    }
    cobs_decode_in_place(slice::from_raw_parts_mut(buffer, len))
        .map(|decoded| decoded as isize)
        .unwrap_or(-1)
}
