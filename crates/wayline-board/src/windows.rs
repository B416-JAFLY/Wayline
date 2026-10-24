use std::os::windows::{fs::OpenOptionsExt, io::IntoRawHandle};
use std::{io, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::NamedPipeClient,
};
use wayline_proto::transfer::{
    Transfer, FRAME_LEN, HEADER_LEN, LABEL_HEIGHT, LABEL_LEN, LABEL_WIDTH,
};
use windows_sys::Win32::{Foundation::RECT, Graphics::Gdi::*};

pub const PIPE: &str = r"\\.\pipe\wayline-radio-ui";

fn open_pipe() -> io::Result<NamedPipeClient> {
    // Match OWL's client-only ACL. GENERIC_WRITE additionally requests the server-creation
    // right, which a normal (unelevated) user deliberately does not have.
    let file = std::fs::OpenOptions::new()
        .access_mode(0x12019b)
        .custom_flags(0x40000000 | 0x00100000 | 0x00010000) // OVERLAPPED, SQOS_PRESENT, IDENTIFICATION
        .open(PIPE)?;
    // SAFETY: a valid uniquely-owned overlapped pipe handle; Tokio takes ownership even on error.
    unsafe { NamedPipeClient::from_raw_handle(file.into_raw_handle()) }
}

async fn exchange(request: &[u8], response: &mut [u8]) -> io::Result<()> {
    tokio::time::timeout(Duration::from_millis(800), async {
        let mut pipe = loop {
            match open_pipe() {
                Ok(pipe) => break pipe,
                Err(error) if error.raw_os_error() == Some(231) => {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                Err(error) => return Err(error),
            }
        };
        pipe.write_all(request).await?;
        let status = pipe.read_u8().await?;
        if status != 0 {
            let _ = pipe.write_all(&[0xff]).await;
            return Err(io::Error::other("board UI request unavailable"));
        }
        pipe.read_exact(response).await?;
        pipe.write_all(&[0xff]).await?;
        Ok(())
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "board UI timeout"))?
}

pub(crate) async fn publish(frame: &[u8; FRAME_LEN]) -> io::Result<()> {
    let mut request = [0; FRAME_LEN + 1];
    request[1..].copy_from_slice(frame);
    exchange(&request, &mut []).await
}

/// Read the firmware's current state through OWL without taking its serial port.
pub fn query(direction: u8) -> io::Result<Transfer> {
    if direction > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "direction must be 0 or 1",
        ));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut header = [0; HEADER_LEN];
    runtime.block_on(exchange(&[1, direction], &mut header))?;
    Transfer::decode(&header)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid board state"))
}

/// Native Windows font coverage allows arbitrary Unicode device names without a large MCU font.
pub fn raster_label(peer: &str) -> io::Result<[u8; LABEL_LEN]> {
    let text: Vec<u16> = peer
        .chars()
        .filter(|c| !c.is_control())
        .take(128)
        .collect::<String>()
        .encode_utf16()
        .collect();
    let family: Vec<u16> = "Microsoft YaHei UI\0".encode_utf16().collect();
    // SAFETY: all GDI objects and their selected-object lifetimes stay on this worker thread.
    unsafe {
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        if dc.is_null() {
            return Err(io::Error::last_os_error());
        }
        let font = CreateFontW(
            -13,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            ANTIALIASED_QUALITY as u32,
            0,
            family.as_ptr(),
        );
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: LABEL_WIDTH as i32,
                biHeight: -(LABEL_HEIGHT as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            },
            ..std::mem::zeroed()
        };
        let mut pixels = std::ptr::null_mut();
        let bitmap = CreateDIBSection(
            dc,
            &info,
            DIB_RGB_COLORS,
            &mut pixels,
            std::ptr::null_mut(),
            0,
        );
        if bitmap.is_null() || font.is_null() || pixels.is_null() {
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            if !font.is_null() {
                DeleteObject(font);
            }
            DeleteDC(dc);
            return Err(io::Error::last_os_error());
        }
        let old_bitmap = SelectObject(dc, bitmap);
        let old_font = SelectObject(dc, font);
        let bytes =
            std::slice::from_raw_parts_mut(pixels.cast::<u8>(), LABEL_WIDTH * LABEL_HEIGHT * 4);
        bytes.fill(0);
        SetBkColor(dc, 0);
        SetTextColor(dc, 0xffffff);
        SetBkMode(dc, TRANSPARENT as i32);
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: LABEL_WIDTH as i32,
            bottom: LABEL_HEIGHT as i32,
        };
        DrawTextW(
            dc,
            text.as_ptr(),
            text.len() as i32,
            &mut rect,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        GdiFlush();
        let mut out = [0; LABEL_LEN];
        for (index, pair) in bytes.as_chunks::<8>().0.iter().enumerate() {
            out[index] =
                (((pair[0] as u16 + 8) / 17) as u8) << 4 | ((pair[4] as u16 + 8) / 17) as u8;
        }
        SelectObject(dc, old_font);
        SelectObject(dc, old_bitmap);
        DeleteObject(font);
        DeleteObject(bitmap);
        DeleteDC(dc);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_labels_are_rasterized_and_clipped_without_control_characters() {
        let english = raster_label("iPhone").unwrap();
        let chinese = raster_label("小明的 iPhone").unwrap();
        assert!(english.iter().any(|b| *b != 0));
        assert_ne!(english, chinese);
        assert_eq!(raster_label("iPhone\n\t").unwrap(), english);
        assert!(raster_label(&"很长的设备名称".repeat(50))
            .unwrap()
            .iter()
            .any(|b| *b != 0));
    }
}
