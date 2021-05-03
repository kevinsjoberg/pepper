use std::{
    env, fs, io,
    os::unix::{
        ffi::OsStrExt,
        io::{AsRawFd, RawFd},
        net::{UnixListener, UnixStream},
    },
    process::Child,
    sync::{
        atomic::{AtomicIsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use pepper::{
    application::{AnyError, ApplicationEvent, ClientApplication, ServerApplication},
    client::ClientHandle,
    platform::{BufPool, Key, Platform, PlatformRequest, ProcessHandle, ProcessTag, SharedBuf},
    Args,
};

mod unix_utils;
use unix_utils::{get_terminal_size, parse_terminal_keys, run, Process, RawMode};

const MAX_CLIENT_COUNT: usize = 20;
const MAX_PROCESS_COUNT: usize = 42;
const MAX_EVENT_COUNT: usize = 1 + 1 + MAX_CLIENT_COUNT + MAX_PROCESS_COUNT;
const _ASSERT_MAX_EVENT_COUNT_IS_64: [(); 64] = [(); MAX_EVENT_COUNT];
const MAX_TRIGGERED_EVENT_COUNT: usize = 32;

pub fn main() {
    static KQUEUE_FD: AtomicIsize = AtomicIsize::new(-1);

    let raw_mode = RawMode::enter();
    let stdin = io::stdin();
    let mut stdin = stdin.lock();

    //let mut buf = [0; 64];
    let mut buf = [0; 1];
    let mut keys = Vec::new();

    let kqueue = Kqueue::new();
    kqueue.add(Event::FlushRequests(false), 0);
    kqueue.add(Event::Fd(stdin.as_raw_fd()), 1);
    kqueue.add(Event::Resize, 2);
    let mut kqueue_events = KqueueEvents::new();

    KQUEUE_FD.store(kqueue.as_raw_fd() as _, Ordering::Relaxed);
    
    std::thread::spawn(|| {
        for _ in 0..10 {
            print!("sending flush request\r\n");
            let fd = KQUEUE_FD.load(Ordering::Relaxed) as _;
            let event = Event::FlushRequests(true).into_kevent(libc::EV_ADD, 0);
            if !modify_kqueue(fd, &event) {
                print!("error trigerring flush events\r\n");
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });

    let (width, height) = get_terminal_size();
    print!("terminal size: {}, {}\r\n", width, height);

    //'main_loop: loop {
    'main_loop: for _ in 0..30 {
        print!("waiting for events...\r\n");
        let events = kqueue.wait(&mut kqueue_events, None);
        for event in events {
            match event.index {
                Ok(0) => {
                    print!("received flush request\r\n");
                }
                Ok(1) => {
                    use io::Read;
                    let len = stdin.read(&mut buf).unwrap();
                    keys.clear();
                    parse_terminal_keys(&buf[..len], &mut keys);
                    for &key in &keys {
                        print!("{}\r\n", key);
                        if key == Key::Char('q') {
                            break 'main_loop;
                        }
                    }
                }
                Ok(2) => {
                    let (width, height) = get_terminal_size();
                    print!("terminal size: {}, {}\r\n", width, height);
                }
                Ok(_) => unreachable!(),
                Err(()) => {
                    panic!("ops something bad happened")
                }
            };
        }
    }

    drop(raw_mode);
    //run(run_server, run_client);
}

enum Event {
    Resize,
    FlushRequests(bool),
    Fd(RawFd),
}
impl Event {
    pub fn into_kevent(self, flags: u16, index: usize) -> libc::kevent {
        match self {
            Self::Resize => libc::kevent {
                ident: libc::SIGWINCH as _,
                filter: libc::EVFILT_SIGNAL,
                flags,
                fflags: 0,
                data: 0,
                udata: index as _,
            },
            Self::FlushRequests(triggered) => libc::kevent {
                ident: 0,
                filter: libc::EVFILT_USER,
                flags: flags | libc::EV_ONESHOT,
                fflags: if triggered { libc::NOTE_TRIGGER } else { 0 },
                data: 0,
                udata: index as _,
            },
            Self::Fd(fd) => libc::kevent {
                ident: fd as _,
                filter: libc::EVFILT_READ,
                flags,
                fflags: 0,
                data: 0,
                udata: index as _,
            },
        }
    }
}

struct TriggeredEvent {
    pub index: usize,
    pub data: isize,
}

struct KqueueEvents([libc::kevent; MAX_TRIGGERED_EVENT_COUNT]);
impl KqueueEvents {
    pub fn new() -> Self {
        const DEFAULT_KEVENT: libc::kevent = libc::kevent {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        Self([DEFAULT_KEVENT; MAX_TRIGGERED_EVENT_COUNT])
    }
}

fn modify_kqueue(fd: RawFd, event: &libc::kevent) -> bool {
    unsafe { libc::kevent(fd, event as _, 1, std::ptr::null_mut(), 0, std::ptr::null()) == 0 }
}

struct Kqueue(RawFd);
impl Kqueue {
    pub fn new() -> Self {
        let fd = unsafe { libc::kqueue() };
        if fd == -1 {
            panic!("could not create kqueue");
        }
        Self(fd)
    }

    pub fn add(&self, event: Event, index: usize) {
        let event = event.into_kevent(libc::EV_ADD, index);
        if !modify_kqueue(self.0, &event) {
            panic!("could not add event");
        }
    }

    pub fn remove(&self, event: Event) {
        let event = event.into_kevent(libc::EV_DELETE, 0);
        if !modify_kqueue(self.0, &event) {
            panic!("could not remove event");
        }
    }

    pub fn wait<'a>(
        &self,
        events: &'a mut KqueueEvents,
        timeout: Option<Duration>,
    ) -> impl 'a + ExactSizeIterator<Item = Result<TriggeredEvent, ()>> {
        let mut timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let timeout = match timeout {
            Some(duration) => {
                timespec.tv_sec = duration.as_secs() as _;
                timespec.tv_nsec = duration.subsec_nanos() as _;
                &timespec as _
            }
            None => std::ptr::null(),
        };

        let len = unsafe {
            libc::kevent(
                self.fd,
                [].as_ptr(),
                0,
                events.0.as_mut_ptr(),
                events.0.len() as _,
                timeout,
            )
        };
        if len == -1 {
            panic!("could not wait for events");
        }

        events.0[..len as usize].iter().map(|e| {
            if e.flags & libc::EV_ERROR != 0 {
                Err(())
            } else {
                Ok(TriggeredEvent {
                    index: e.udata as _,
                    data: e.data as _,
                })
            }
        })
    }
}
impl AsRawFd for Kqueue {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}
impl Drop for Kqueue {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

fn run_server(listener: UnixListener) -> Result<(), AnyError> {
    use io::{Read, Write};

    const NONE_PROCESS: Option<Process> = None;

    let (request_sender, request_receiver) = mpsc::channel();
    let platform = Platform::new(|| (), request_sender);
    let event_sender = ServerApplication::run(platform);

    let mut client_connections: [Option<UnixStream>; MAX_CLIENT_COUNT] = Default::default();
    let mut processes = [NONE_PROCESS; MAX_PROCESS_COUNT];
    let mut buf_pool = BufPool::default();

    let (request_sender, request_receiver) = mpsc::channel();
    let platform = Platform::new(|| (), request_sender);
    let event_sender = ServerApplication::run(platform);

    let mut timeout = Some(ServerApplication::idle_duration());

    loop {
        return Ok(());
    }
}

fn run_client(args: Args, mut connection: UnixStream) {
    use io::{Read, Write};

    let stdin = io::stdin();
    let mut stdin = stdin.lock();

    let mut client_index = 0;
    match connection.read(std::slice::from_mut(&mut client_index)) {
        Ok(1) => (),
        _ => return,
    }

    let client_handle = ClientHandle::from_index(client_index as _).unwrap();
    let is_pipped = unsafe { libc::isatty(stdin.as_raw_fd()) == 0 };

    let stdout = io::stdout();
    let mut application = ClientApplication::new(client_handle, stdout.lock(), is_pipped);
    let bytes = application.init(args);
    if connection.write(bytes).is_err() {
        return;
    }

    let raw_mode;

    if is_pipped {
        raw_mode = None;
    } else {
        raw_mode = Some(RawMode::enter());

        let size = get_terminal_size();
        let bytes = application.update(Some(size), &[], &[], &[]);
        if connection.write(bytes).is_err() {
            return;
        }
    }

    //let mut keys = Vec::new();
    let mut stream_buf = [0; ClientApplication::connection_buffer_len()];
    let mut stdin_buf = [0; ClientApplication::stdin_buffer_len()];

    loop {
        break;
    }

    drop(raw_mode);
}
