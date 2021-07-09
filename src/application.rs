use std::{env, fmt, fs, io, panic, path::Path, sync::mpsc, time::Duration};

use crate::{
    buffer::parse_path_and_position,
    client::{ClientHandle, ClientManager},
    editor::{Editor, EditorControlFlow},
    editor_utils::{load_config, MessageKind},
    events::{ClientEvent, ClientEventReceiver, ServerEvent},
    ini::Ini,
    platform::{Key, Platform, PlatformRequest, ProcessHandle, ProcessTag, SharedBuf},
    serialization::{DeserializeError, Serialize},
    ui, Args,
};

pub struct AnyError;
impl<T> From<T> for AnyError
where
    T: std::error::Error,
{
    fn from(_: T) -> Self {
        Self
    }
}

pub enum ApplicationEvent {
    Idle,
    Redraw,
    ConnectionOpen {
        handle: ClientHandle,
    },
    ConnectionClose {
        handle: ClientHandle,
    },
    ConnectionOutput {
        handle: ClientHandle,
        buf: SharedBuf,
    },
    ProcessSpawned {
        tag: ProcessTag,
        handle: ProcessHandle,
    },
    ProcessOutput {
        tag: ProcessTag,
        buf: SharedBuf,
    },
    ProcessExit {
        tag: ProcessTag,
    },
}

pub struct ApplicationEventSender(mpsc::Sender<ApplicationEvent>);
impl ApplicationEventSender {
    pub fn send(&self, event: ApplicationEvent) -> Result<(), AnyError> {
        match self.0.send(event) {
            Ok(()) => Ok(()),
            Err(_) => Err(AnyError),
        }
    }
}

pub struct ServerApplication;
impl ServerApplication {
    pub fn platform_request_channel() -> (
        mpsc::Sender<PlatformRequest>,
        mpsc::Receiver<PlatformRequest>,
    ) {
        mpsc::channel()
    }

    pub const fn connection_buffer_len() -> usize {
        512
    }

    pub const fn idle_duration() -> Duration {
        Duration::from_secs(1)
    }

    pub fn run(args: Args, mut platform: Platform) -> Option<ApplicationEventSender> {
        let current_dir = env::current_dir().expect("could not retrieve the current directory");
        let mut editor = Editor::new(current_dir);

        let mut ini = Ini::default();
        if !args.no_default_config {
            let source = include_str!("../rc/default_config.ini");
            load_config(
                &mut editor,
                &mut platform,
                &mut ini,
                "default_config.ini",
                source,
            );
        }

        for config in args.configs {
            let path = Path::new(&config.path);
            if config.suppress_file_not_found && !path.exists() {
                continue;
            }
            match fs::read_to_string(path) {
                Ok(source) => {
                    load_config(&mut editor, &mut platform, &mut ini, &config.path, &source)
                }
                Err(_) => editor
                    .status_bar
                    .write(MessageKind::Error)
                    .fmt(format_args!("could not load config '{}'", config.path)),
            }
        }

        let (event_sender, event_receiver) = mpsc::channel();
        let application_event_sender = ApplicationEventSender(event_sender.clone());
        std::thread::spawn(move || {
            let _ = Self::run_application(editor, &mut platform, event_sender, event_receiver);
            platform.enqueue_request(PlatformRequest::Quit);
            platform.flush_requests();
        });

        Some(application_event_sender)
    }

    fn run_application(
        mut editor: Editor,
        platform: &mut Platform,
        event_sender: mpsc::Sender<ApplicationEvent>,
        event_receiver: mpsc::Receiver<ApplicationEvent>,
    ) -> Result<(), AnyError> {
        let mut clients = ClientManager::default();
        let mut is_first_client = true;
        let mut client_event_receiver = ClientEventReceiver::default();

        'event_loop: loop {
            let mut event = event_receiver.recv()?;
            loop {
                match event {
                    ApplicationEvent::Idle => editor.on_idle(&mut clients, platform),
                    ApplicationEvent::Redraw => (),
                    ApplicationEvent::ConnectionOpen { handle } => {
                        clients.on_client_joined(handle);
                        let mut buf = platform.buf_pool.acquire();
                        let write = buf.write();
                        write.push(is_first_client as _);
                        write.push(handle.into_index() as _);
                        let buf = buf.share();
                        platform.buf_pool.release(buf.clone());
                        platform.enqueue_request(PlatformRequest::WriteToClient { handle, buf });
                        is_first_client = false;
                    }
                    ApplicationEvent::ConnectionClose { handle } => {
                        clients.on_client_left(handle);
                        if clients.iter().next().is_none() {
                            break 'event_loop;
                        }
                    }
                    ApplicationEvent::ConnectionOutput { handle, buf } => {
                        let mut events =
                            client_event_receiver.receive_events(handle, buf.as_bytes());
                        while let Some(event) = events.next(&client_event_receiver) {
                            match editor.on_client_event(platform, &mut clients, event) {
                                EditorControlFlow::Continue => (),
                                EditorControlFlow::Suspend => {
                                    let mut buf = platform.buf_pool.acquire();
                                    let write = buf.write();
                                    ServerEvent::Suspend.serialize(write);
                                    let buf = buf.share();
                                    platform.enqueue_request(PlatformRequest::WriteToClient {
                                        handle,
                                        buf,
                                    });
                                }
                                EditorControlFlow::Quit => {
                                    platform
                                        .enqueue_request(PlatformRequest::CloseClient { handle });
                                    break;
                                }
                                EditorControlFlow::QuitAll => break 'event_loop,
                            }
                        }
                        events.finish(&mut client_event_receiver);
                    }
                    ApplicationEvent::ProcessSpawned { tag, handle } => {
                        editor.on_process_spawned(platform, tag, handle)
                    }
                    ApplicationEvent::ProcessOutput { tag, buf } => {
                        editor.on_process_output(platform, &mut clients, tag, buf.as_bytes())
                    }
                    ApplicationEvent::ProcessExit { tag } => {
                        editor.on_process_exit(platform, &mut clients, tag)
                    }
                }

                event = match event_receiver.try_recv() {
                    Ok(event) => event,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return Err(AnyError),
                };
            }

            let needs_redraw = editor.on_pre_render(&mut clients);
            if needs_redraw {
                event_sender.send(ApplicationEvent::Redraw)?;
            }

            let focused_client_handle = clients.focused_client();
            for c in clients.iter() {
                if !c.has_ui() {
                    continue;
                }

                let has_focus = focused_client_handle == Some(c.handle());

                let mut buf = platform.buf_pool.acquire();
                let write = buf.write_with_len(ServerEvent::display_header_len());
                ui::render(
                    &editor,
                    c.buffer_view_handle(),
                    (c.viewport_size.0, c.height),
                    c.scroll as _,
                    has_focus,
                    write,
                );
                ServerEvent::serialize_display_header(write);

                let handle = c.handle();
                let buf = buf.share();
                platform.buf_pool.release(buf.clone());
                platform.enqueue_request(PlatformRequest::WriteToClient { handle, buf });
            }

            platform.flush_requests();
        }

        Ok(())
    }
}

pub struct ClientApplication<'stdout> {
    handle: ClientHandle,
    is_pipped: bool,
    stdin_read_buf: Vec<u8>,
    server_read_buf: Vec<u8>,
    server_write_buf: Vec<u8>,
    stdout: io::StdoutLock<'stdout>,
}
impl<'stdout> ClientApplication<'stdout> {
    pub const fn stdin_buffer_len() -> usize {
        4 * 1024
    }

    pub const fn connection_buffer_len() -> usize {
        48 * 1024
    }

    pub fn new(handle: ClientHandle, stdout: io::StdoutLock<'stdout>, is_pipped: bool) -> Self {
        Self {
            handle,
            is_pipped,
            stdin_read_buf: Vec::new(),
            server_read_buf: Vec::new(),
            server_write_buf: Vec::new(),
            stdout,
        }
    }

    pub fn init<'a>(&'a mut self, args: Args, is_first_client: bool) -> &'a [u8] {
        self.server_write_buf.clear();

        if let Some(handle) = args.as_client {
            self.handle = handle;
        }

        let mut commands = String::new();
        if is_first_client {
            for config in &args.configs {
                use fmt::Write;
                if config.suppress_file_not_found {
                    writeln!(commands, "source '{}'", &config.path).unwrap();
                } else {
                    writeln!(commands, "try-source '{}'", &config.path).unwrap();
                }
            }
        }
        for path in &args.files {
            use fmt::Write;
            let (path, position) = parse_path_and_position(path);
            match position {
                Some(position) => {
                    writeln!(
                        commands,
                        "open '{}' -line={} -column={}",
                        path,
                        position.line_index + 1,
                        position.column_byte_index + 1,
                    )
                    .unwrap();
                }
                None => writeln!(commands, "open '{}'", path).unwrap(),
            }
        }

        self.reinit_screen();
        if !self.is_pipped {
            if args.as_client.is_none() {
                ClientEvent::Key(self.handle, Key::None).serialize(&mut self.server_write_buf);
            }
        }

        if !commands.is_empty() {
            ClientEvent::Command(self.handle, &commands).serialize(&mut self.server_write_buf);
        }

        self.server_write_buf.as_slice()
    }

    pub fn reinit_screen(&mut self) {
        if self.is_pipped {
            return;
        }

        use io::Write;
        let _ = self.stdout.write_all(ui::ENTER_ALTERNATE_BUFFER_CODE);
        let _ = self.stdout.write_all(ui::HIDE_CURSOR_CODE);
        let _ = self.stdout.write_all(ui::MODE_256_COLORS_CODE);
        self.stdout.flush().unwrap();
    }

    pub fn restore_screen(&mut self) {
        if self.is_pipped {
            return;
        }

        use io::Write;
        let _ = self.stdout.write_all(ui::EXIT_ALTERNATE_BUFFER_CODE);
        let _ = self.stdout.write_all(ui::SHOW_CURSOR_CODE);
        let _ = self.stdout.write_all(ui::RESET_STYLE_CODE);
        let _ = self.stdout.flush();
    }

    pub fn update<'a>(
        &'a mut self,
        resize: Option<(usize, usize)>,
        keys: &[Key],
        stdin_bytes: &[u8],
        server_bytes: &[u8],
    ) -> (bool, &'a [u8]) {
        use io::Write;

        self.server_write_buf.clear();

        if let Some((width, height)) = resize {
            ClientEvent::Resize(self.handle, width as _, height as _)
                .serialize(&mut self.server_write_buf);
        }

        for key in keys {
            ClientEvent::Key(self.handle, *key).serialize(&mut self.server_write_buf);
        }

        if !stdin_bytes.is_empty() {
            self.stdin_read_buf.extend_from_slice(stdin_bytes);
            for command in self.stdin_read_buf.split(|&b| b == b'\0') {
                match std::str::from_utf8(command) {
                    Ok(command) => ClientEvent::Command(self.handle, command)
                        .serialize(&mut self.server_write_buf),
                    Err(_) => ClientEvent::Command(
                        self.handle,
                        "print -error 'error parsing utf8 from stdin'",
                    )
                    .serialize(&mut self.server_write_buf),
                }
            }
        }

        let mut suspend = false;
        if !server_bytes.is_empty() {
            self.server_read_buf.extend_from_slice(server_bytes);
            let mut read_slice = &self.server_read_buf[..];

            loop {
                let previous_slice = read_slice;
                match ServerEvent::deserialize(&mut read_slice) {
                    Ok(ServerEvent::Display(display)) => self.stdout.write_all(display).unwrap(),
                    Ok(ServerEvent::Suspend) => suspend = true,
                    Ok(ServerEvent::CommandOutput(output)) => {
                        self.stdout.write_all(output.as_bytes()).unwrap();
                        self.stdout.write_all(b"\0").unwrap();
                    }
                    Ok(ServerEvent::Request(_)) => (),
                    Err(DeserializeError::InsufficientData) => {
                        let read_len = self.server_read_buf.len() - previous_slice.len();
                        self.server_read_buf.drain(..read_len);
                        break;
                    }
                    Err(DeserializeError::InvalidData) => {
                        panic!("client received invalid data from server")
                    }
                }
            }

            self.stdout.flush().unwrap();
        }

        (suspend, self.server_write_buf.as_slice())
    }
}
impl<'stdout> Drop for ClientApplication<'stdout> {
    fn drop(&mut self) {
        self.restore_screen();
    }
}

pub fn set_panic_hook() {
    static mut ORIGINAL_PANIC_HOOK: Option<Box<dyn Fn(&panic::PanicInfo) + Sync + Send + 'static>> =
        None;
    unsafe { ORIGINAL_PANIC_HOOK = Some(panic::take_hook()) };

    panic::set_hook(Box::new(|info| unsafe {
        if let Ok(mut file) = fs::File::create("pepper-crash.txt") {
            use io::Write;
            let _ = writeln!(file, "{}", info);
        }

        if let Some(ref hook) = ORIGINAL_PANIC_HOOK {
            hook(info);
        }
    }));
}
