//! UI-only pipe compatible with wayline-board. Errors never stop AWDL.
use crate::serial::Control;
use std::{io, mem, ptr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::watch,
};
use wayline_proto::transfer::FRAME_LEN;
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    },
};

pub const PIPE: &str = r"\\.\pipe\wayline-radio-ui";

fn listen(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    let sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;0x12019b;;;AU)\0"
        .encode_utf16()
        .collect();
    let mut raw: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: terminated SDDL and initialized output pointer live through the call.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut raw,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    struct Descriptor(PSECURITY_DESCRIPTOR);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    let descriptor = Descriptor(raw);
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    // SAFETY: Windows copies the descriptor during creation; attrs and descriptor
    // outlive that synchronous call. The returned server owns the pipe handle.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .in_buffer_size((FRAME_LEN + 1) as u32)
            .out_buffer_size(64)
            .create_with_security_attributes_raw(
                name,
                (&mut attrs as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}

pub async fn serve(
    name: &str,
    control: Control,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let mut listener = listen(name, true)?;
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        tokio::select! {
            _=shutdown.changed()=>return Ok(()),
            connected=listener.connect()=>{ connected?; }
        }
        // Each connected client owns its handle. Never reuse a Tokio pipe with
        // stale overlapped readiness from the preceding client's disconnect.
        let next = listen(name, false)?;
        let mut pipe = std::mem::replace(&mut listener, next);
        tokio::select! {
            _=shutdown.changed()=>return Ok(()),
            _=tokio::time::timeout(Duration::from_millis(750),exchange(&mut pipe,&control))=>{}
        }
        let _ = pipe.disconnect();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn exchange(pipe: &mut NamedPipeServer, control: &Control) -> io::Result<()> {
    let result = match pipe.read_u8().await? {
        0 => {
            let mut frame = [0; FRAME_LEN];
            pipe.read_exact(&mut frame).await?;
            control.publish(&frame).map(|_| Vec::new())
        }
        1 => {
            let direction = pipe.read_u8().await?;
            control.query(direction).await
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid board operation",
        )),
    };
    match result {
        Ok(body) => {
            pipe.write_u8(0).await?;
            pipe.write_all(&body).await?;
        }
        Err(_) => pipe.write_u8(1).await?,
    }
    if pipe.read_u8().await? != 0xff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing board reply acknowledgment",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serial::tests::{encoded, fixture};
    use std::os::windows::{fs::OpenOptionsExt, io::IntoRawHandle};
    use tokio::net::windows::named_pipe::NamedPipeClient;
    use wayline_proto::transfer::{Transfer, FRAME_LEN, HEADER_LEN};

    #[tokio::test]
    async fn real_pipe_preserves_board_protocol_and_rejects_invalid_operations() {
        let (radio, input, output) = fixture();
        let name = format!(r"\\.\pipe\wayline-board-test-{}", std::process::id());
        let (stop, shutdown) = watch::channel(false);
        let control = radio.control();
        let server_name = name.clone();
        let server = tokio::spawn(async move { serve(&server_name, control, shutdown).await });
        async fn connect(name: &str) -> NamedPipeClient {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    match std::fs::OpenOptions::new()
                        .access_mode(0x12019b)
                        .custom_flags(0x40000000 | 0x00100000 | 0x00010000)
                        .open(name)
                    {
                        Ok(file) => {
                            // SAFETY: Tokio takes ownership of this overlapped handle.
                            break unsafe {
                                NamedPipeClient::from_raw_handle(file.into_raw_handle())
                            }
                            .unwrap();
                        }
                        Err(e) if matches!(e.raw_os_error(), Some(2 | 231)) => {
                            tokio::time::sleep(Duration::from_millis(5)).await
                        }
                        Err(e) => panic!("pipe connect: {e}"),
                    }
                }
            })
            .await
            .unwrap()
        }
        let mut client = connect(&name).await;
        client.write_u8(99).await.unwrap();
        assert_eq!(client.read_u8().await.unwrap(), 1);
        client.write_u8(0xff).await.unwrap();
        drop(client);
        let mut client = connect(&name).await;
        let mut msg = vec![0; FRAME_LEN + 1];
        msg[1..1 + HEADER_LEN].copy_from_slice(&Transfer::idle(0).encode());
        client.write_all(&msg).await.unwrap();
        assert_eq!(client.read_u8().await.unwrap(), 0);
        client.write_u8(0xff).await.unwrap();
        drop(client);
        let mut client = connect(&name).await;
        client.write_all(&[1, 0]).await.unwrap();
        let query = encoded(&[wayline_proto::HostMessage::TransferStatus as u8, 0]);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !output.lock().unwrap().ends_with(&query) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        let mut response = vec![wayline_proto::RadioMessage::TransferResponse as u8];
        response.extend(Transfer::idle(0).encode());
        input.send(encoded(&response)).unwrap();
        assert_eq!(client.read_u8().await.unwrap(), 0);
        let mut header = [0; HEADER_LEN];
        client.read_exact(&mut header).await.unwrap();
        assert_eq!(Transfer::decode(&header), Some(Transfer::idle(0)));
        client.write_u8(0xff).await.unwrap();
        drop(client);
        let _idle = connect(&name).await;
        stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(radio);
    }
}
