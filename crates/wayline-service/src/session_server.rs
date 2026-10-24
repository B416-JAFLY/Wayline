use crate::{
    client, server,
    session::{Hello, Release, Reply, SessionKind, VERSION},
};
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::NamedPipeServer,
    sync::{mpsc, oneshot, watch, Semaphore},
    task::JoinSet,
};
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::{
        Pipes::GetNamedPipeClientProcessId,
        Threading::{OpenProcess, WaitForSingleObject, SYNCHRONIZATION_SYNCHRONIZE},
    },
};

pub(crate) enum Event {
    Acquire {
        kind: SessionKind,
        response: oneshot::Sender<Reply>,
    },
    Release {
        token: u64,
        ready: bool,
        response: Option<oneshot::Sender<Reply>>,
    },
    ConfirmDrain,
}
struct Connection(NamedPipeServer);
impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.0.disconnect();
    }
}
struct Registration {
    token: u64,
    events: mpsc::UnboundedSender<Event>,
    released: bool,
}
impl Drop for Registration {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.events.send(Event::Release {
                token: self.token,
                ready: false,
                response: None,
            });
        }
    }
}
async fn send(pipe: &mut NamedPipeServer, reply: &Reply) -> io::Result<()> {
    let bytes = client::encode(reply).map_err(io::Error::other)?;
    tokio::time::timeout(server::CLIENT_TIMEOUT, pipe.write_all(&bytes))
        .await
        .map_err(io::Error::other)??;
    Ok(())
}
async fn reject(pipe: &mut NamedPipeServer, reply: &Reply) -> io::Result<()> {
    send(pipe, reply).await?;
    let mut byte = [0];
    let _ = tokio::time::timeout(server::CLIENT_TIMEOUT, pipe.read(&mut byte)).await;
    Ok(())
}
async fn client_session(
    pipe: NamedPipeServer,
    events: mpsc::UnboundedSender<Event>,
) -> io::Result<()> {
    let mut connection = Connection(pipe);
    let pipe = &mut connection.0;
    let hello: Hello = tokio::time::timeout(server::CLIENT_TIMEOUT, client::read_json(pipe))
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    if hello.version != VERSION {
        reject(
            pipe,
            &Reply::error(
                "upgrade",
                "unsupported session protocol; update host and service together",
            ),
        )
        .await?;
        return Ok(());
    }
    let mut pid = 0;
    // SAFETY: pipe is a live local named-pipe server; pid points to writable storage.
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid) } == 0
        || pid != hello.pid
    {
        reject(
            pipe,
            &Reply::error(
                "identity",
                "session PID does not match the connected process",
            ),
        )
        .await?;
        return Ok(());
    }
    // SAFETY: only synchronization access is requested to an OS-reported PID.
    let process = unsafe { OpenProcess(SYNCHRONIZATION_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        reject(pipe, &Reply::error("identity", io::Error::last_os_error())).await?;
        return Ok(());
    }
    // SAFETY: successful OpenProcess transfers one owned handle to this scope.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let (response, result) = oneshot::channel();
    events
        .send(Event::Acquire {
            kind: hello.kind,
            response,
        })
        .map_err(io::Error::other)?;
    let reply = result.await.map_err(io::Error::other)?;
    if reply.error.is_some() {
        reject(pipe, &reply).await?;
        return Ok(());
    }
    let mut registration = Registration {
        token: reply.token,
        events: events.clone(),
        released: false,
    };
    send(pipe, &reply).await?;
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    let release = {
        let release = client::read_json::<Release>(pipe);
        tokio::pin!(release);
        loop {
            tokio::select! {
                result=&mut release=>break result.map_err(io::Error::other)?,
                _=timer.tick()=>{
                    // SAFETY: process is owned and remains valid throughout the wait.
                    if unsafe { WaitForSingleObject(process.as_raw_handle(),0) } == WAIT_OBJECT_0 { return Ok(()); }
                }
            }
        }
    };
    if release.instance != reply.instance || release.token != reply.token {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session token belongs to another connection or service instance",
        ));
    }
    let (response, result) = oneshot::channel();
    events
        .send(Event::Release {
            token: reply.token,
            ready: release.device_ready,
            response: Some(response),
        })
        .map_err(io::Error::other)?;
    registration.released = true;
    let reply = result.await.map_err(io::Error::other)?;
    // End the read future's borrow before writing a release confirmation.
    send(&mut connection.0, &reply).await?;
    let mut ack = [0];
    let _ = tokio::time::timeout(server::CLIENT_TIMEOUT, connection.0.read(&mut ack)).await;
    if reply.outcome.service_stopping {
        let _ = events.send(Event::ConfirmDrain);
    }
    Ok(())
}
pub(crate) async fn serve(
    name: String,
    mut listener: NamedPipeServer,
    events: mpsc::UnboundedSender<Event>,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let mut clients = JoinSet::new();
    let limit = Arc::new(Semaphore::new(64));
    let result = loop {
        if *shutdown.borrow() {
            break Ok(());
        }
        tokio::select! {
            _=shutdown.changed()=>break Ok(()),
            Some(_)=clients.join_next(),if !clients.is_empty()=>{},
            connected=listener.connect()=>{
                if connected.is_err() { let _=listener.disconnect();continue; }
                let next=match server::listen(&name,false) { Ok(next)=>next,Err(e)=>break Err(e) };
                let pipe=std::mem::replace(&mut listener,next);
                if let Ok(permit)=limit.clone().try_acquire_owned() {
                    let events=events.clone();
                    clients.spawn(async move { let _permit=permit; let _=client_session(pipe,events).await; });
                } else { let _=pipe.disconnect(); }
            }
        }
    };
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    let _ = listener.disconnect();
    result
}
