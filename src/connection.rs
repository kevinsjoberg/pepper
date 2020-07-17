use std::{io, path::Path};

use uds_windows::{UnixListener, UnixStream};

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use smol::Async;

use crate::event::Key;

pub struct ClientListener {
    listener: Async<UnixListener>,
}

impl ClientListener {
    pub fn listen<P>(path: P) -> io::Result<Self>
    where
        P: AsRef<Path>,
    {
        Ok(Self {
            listener: Async::new(UnixListener::bind(path)?)?,
        })
    }

    pub async fn accept(&self) -> io::Result<UnixStream> {
        self.listener.read_with(|l| Ok(l.accept()?.0)).await
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum TargetClient {
    All,
    Local,
    Remote(RemoteConnectionWithClientHandle),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RemoteConnectionWithClientHandle(usize);

#[derive(Default)]
pub struct RemoteConnectionWithClientCollection {
    connections: Vec<Option<RemoteConnectionWithClient>>,
    free_slots: Vec<RemoteConnectionWithClientHandle>,
}

impl RemoteConnectionWithClientCollection {
    pub fn add(
        &mut self,
        connection: RemoteConnectionWithClient,
    ) -> RemoteConnectionWithClientHandle {
        if let Some(handle) = self.free_slots.pop() {
            self.connections[handle.0] = Some(connection);
            handle
        } else {
            let index = self.connections.len();
            self.connections.push(Some(connection));
            RemoteConnectionWithClientHandle(index)
        }
    }

    pub fn remove(&mut self, handle: RemoteConnectionWithClientHandle) {
        self.connections[handle.0] = None;
        self.free_slots.push(handle);
    }

    pub fn get(
        &self,
        handle: RemoteConnectionWithClientHandle,
    ) -> Option<&RemoteConnectionWithClient> {
        self.connections[handle.0].as_ref()
    }
}

pub fn local_connection() -> (LocalConnectionWithClient, LocalConnectionWithServer) {
    let (key_sender, key_receiver) = unbounded();
    let (command_sender, command_receiver) = unbounded();

    let client = LocalConnectionWithClient {
        command_sender,
        key_receiver,
    };
    let server = LocalConnectionWithServer {
        key_sender,
        command_receiver,
    };

    (client, server)
}

pub struct LocalConnectionWithClient {
    //pub command_sender: UnboundedSender<EditorOperation>,
    pub command_sender: UnboundedSender<()>,
    pub key_receiver: UnboundedReceiver<Key>,
}

pub struct LocalConnectionWithServer {
    pub key_sender: UnboundedSender<Key>,
    //pub command_receiver: UnboundedReceiver<EditorOperation>,
    pub command_receiver: UnboundedReceiver<()>,
}

pub struct RemoteConnectionWithClient {}
pub struct RemoteConnectionWithServer {}
