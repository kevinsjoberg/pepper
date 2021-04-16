use std::{
    fmt,
    fs::File,
    io,
    ops::Range,
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    str::FromStr,
};

use crate::{
    buffer::{Buffer, BufferCapabilities, BufferContent, BufferHandle},
    buffer_position::{BufferPosition, BufferRange},
    buffer_view::BufferViewHandle,
    client,
    command::parse_process_command,
    cursor::Cursor,
    editor::Editor,
    editor_utils::{MessageKind, StatusBar},
    events::{EditorEvent, EditorEventIter},
    glob::{Glob, InvalidGlobError},
    json::{
        FromJson, Json, JsonArray, JsonConvertError, JsonInteger, JsonObject, JsonString, JsonValue,
    },
    lsp::{
        capabilities,
        protocol::{
            self, DocumentCodeAction, DocumentDiagnostic, DocumentLocation, DocumentPosition,
            DocumentRange, DocumentSymbolInformation, PendingRequestColection, Protocol,
            ResponseError, ServerEvent, ServerNotification, ServerRequest, ServerResponse,
            TextEdit, Uri, WorkspaceEdit,
        },
    },
    mode::{picker, read_line, ModeContext},
    platform::{Platform, PlatformRequest, ProcessHandle, ProcessTag},
};

#[derive(Default)]
struct GenericCapability(pub bool);
impl<'json> FromJson<'json> for GenericCapability {
    fn from_json(value: JsonValue, _: &'json Json) -> Result<Self, JsonConvertError> {
        match value {
            JsonValue::Null => Ok(Self(false)),
            JsonValue::Boolean(b) => Ok(Self(b)),
            JsonValue::Object(_) => Ok(Self(true)),
            _ => Err(JsonConvertError),
        }
    }
}

#[derive(Default)]
struct TriggerCharactersCapability {
    pub on: bool,
    pub trigger_characters: String,
}
impl<'json> FromJson<'json> for TriggerCharactersCapability {
    fn from_json(value: JsonValue, json: &'json Json) -> Result<Self, JsonConvertError> {
        match value {
            JsonValue::Null => Ok(Self {
                on: false,
                trigger_characters: String::new(),
            }),
            JsonValue::Object(options) => {
                let mut trigger_characters = String::new();
                for c in options.get("triggerCharacters".into(), json).elements(json) {
                    if let JsonValue::String(c) = c {
                        let c = c.as_str(json);
                        trigger_characters.push_str(c);
                    }
                }
                Ok(Self {
                    on: true,
                    trigger_characters,
                })
            }
            _ => Err(JsonConvertError),
        }
    }
}

#[derive(Default)]
struct RenameCapability {
    pub on: bool,
    pub prepare_provider: bool,
}
impl<'json> FromJson<'json> for RenameCapability {
    fn from_json(value: JsonValue, json: &'json Json) -> Result<Self, JsonConvertError> {
        match value {
            JsonValue::Null => Ok(Self {
                on: false,
                prepare_provider: false,
            }),
            JsonValue::Boolean(b) => Ok(Self {
                on: b,
                prepare_provider: false,
            }),
            JsonValue::Object(options) => Ok(Self {
                on: true,
                prepare_provider: matches!(
                    options.get("prepareProvider", &json),
                    JsonValue::Boolean(true)
                ),
            }),
            _ => Err(JsonConvertError),
        }
    }
}

enum TextDocumentSyncKind {
    None,
    Full,
    Incremental,
}
struct TextDocumentSyncCapability {
    pub open_close: bool,
    pub change: TextDocumentSyncKind,
    pub save: TextDocumentSyncKind,
}
impl Default for TextDocumentSyncCapability {
    fn default() -> Self {
        Self {
            open_close: false,
            change: TextDocumentSyncKind::None,
            save: TextDocumentSyncKind::None,
        }
    }
}
impl<'json> FromJson<'json> for TextDocumentSyncCapability {
    fn from_json(value: JsonValue, json: &'json Json) -> Result<Self, JsonConvertError> {
        match value {
            JsonValue::Integer(0) => Ok(Self {
                open_close: false,
                change: TextDocumentSyncKind::None,
                save: TextDocumentSyncKind::None,
            }),
            JsonValue::Integer(1) => Ok(Self {
                open_close: true,
                change: TextDocumentSyncKind::Full,
                save: TextDocumentSyncKind::Full,
            }),
            JsonValue::Integer(2) => Ok(Self {
                open_close: true,
                change: TextDocumentSyncKind::Incremental,
                save: TextDocumentSyncKind::Incremental,
            }),
            JsonValue::Object(options) => {
                let mut open_close = false;
                let mut change = TextDocumentSyncKind::None;
                let mut save = TextDocumentSyncKind::None;
                for (key, value) in options.members(json) {
                    match key {
                        "change" => {
                            change = match value {
                                JsonValue::Integer(0) => TextDocumentSyncKind::None,
                                JsonValue::Integer(1) => TextDocumentSyncKind::Full,
                                JsonValue::Integer(2) => TextDocumentSyncKind::Incremental,
                                _ => return Err(JsonConvertError),
                            }
                        }
                        "openClose" => {
                            open_close = match value {
                                JsonValue::Boolean(b) => b,
                                _ => return Err(JsonConvertError),
                            }
                        }
                        "save" => {
                            save = match value {
                                JsonValue::Boolean(false) => TextDocumentSyncKind::None,
                                JsonValue::Boolean(true) => TextDocumentSyncKind::Incremental,
                                JsonValue::Object(options) => {
                                    match options.get("includeText", json) {
                                        JsonValue::Boolean(true) => TextDocumentSyncKind::Full,
                                        _ => TextDocumentSyncKind::Incremental,
                                    }
                                }
                                _ => return Err(JsonConvertError),
                            }
                        }
                        _ => (),
                    }
                }
                Ok(Self {
                    open_close,
                    change,
                    save,
                })
            }
            _ => Err(JsonConvertError),
        }
    }
}

declare_json_object! {
    #[derive(Default)]
    struct ServerCapabilities {
        textDocumentSync: TextDocumentSyncCapability,
        completionProvider: TriggerCharactersCapability,
        hoverProvider: GenericCapability,
        signatureHelpProvider: TriggerCharactersCapability,
        declarationProvider: GenericCapability,
        definitionProvider: GenericCapability,
        implementationProvider: GenericCapability,
        referencesProvider: GenericCapability,
        documentSymbolProvider: GenericCapability,
        codeActionProvider: GenericCapability,
        documentFormattingProvider: GenericCapability,
        renameProvider: RenameCapability,
        workspaceSymbolProvider: GenericCapability,
    }
}

pub struct Diagnostic {
    pub message: String,
    pub range: BufferRange,
    pub data: Vec<u8>,
}
impl Diagnostic {
    pub fn as_document_diagnostic(&self, json: &mut Json) -> DocumentDiagnostic {
        let mut reader = io::Cursor::new(&self.data);
        let data = match json.read(&mut reader) {
            Ok(value) => value,
            Err(_) => JsonValue::Null,
        };
        DocumentDiagnostic {
            message: json.create_string(&self.message),
            range: self.range.into(),
            data,
        }
    }
}

struct BufferDiagnosticCollection {
    path: PathBuf,
    buffer_handle: Option<BufferHandle>,
    diagnostics: Vec<Diagnostic>,
    len: usize,
}
impl BufferDiagnosticCollection {
    pub fn add(&mut self, diagnostic: DocumentDiagnostic, json: &Json) {
        let message = diagnostic.message.as_str(json);
        let range = diagnostic.range.into();

        if self.len < self.diagnostics.len() {
            let diagnostic = &mut self.diagnostics[self.len];
            diagnostic.message.clear();
            diagnostic.message.push_str(message);
            diagnostic.range = range;
            diagnostic.data.clear();
        } else {
            self.diagnostics.push(Diagnostic {
                message: message.into(),
                range: range.into(),
                data: Vec::new(),
            });
        }

        json.write(&mut self.diagnostics[self.len].data, &diagnostic.data);
        self.len += 1;
    }

    pub fn sort(&mut self) {
        self.diagnostics.sort_by_key(|d| d.range.from);
    }
}

fn is_editor_path_equals_to_lsp_path(
    editor_root: &Path,
    editor_path: &Path,
    lsp_root: &Path,
    lsp_path: &Path,
) -> bool {
    let lsp_components = lsp_root.components().chain(lsp_path.components());
    if editor_path.is_absolute() {
        editor_path.components().eq(lsp_components)
    } else {
        editor_root
            .components()
            .chain(editor_path.components())
            .eq(lsp_components)
    }
}

struct VersionedBufferEdit {
    buffer_range: BufferRange,
    text_range: Range<usize>,
}
#[derive(Default)]
struct VersionedBuffer {
    version: usize,
    texts: String,
    pending_edits: Vec<VersionedBufferEdit>,
}
impl VersionedBuffer {
    pub fn flush(&mut self) {
        self.texts.clear();
        self.pending_edits.clear();
        self.version += 1;
    }

    pub fn dispose(&mut self) {
        self.flush();
        self.version = 1;
    }
}
#[derive(Default)]
struct VersionedBufferCollection {
    buffers: Vec<VersionedBuffer>,
}
impl VersionedBufferCollection {
    pub fn add_edit(&mut self, buffer_handle: BufferHandle, range: BufferRange, text: &str) {
        let index = buffer_handle.0 as usize;
        if index >= self.buffers.len() {
            self.buffers
                .resize_with(index + 1, VersionedBuffer::default);
        }
        let buffer = &mut self.buffers[index];
        let text_range_start = buffer.texts.len();
        buffer.texts.push_str(text);
        buffer.pending_edits.push(VersionedBufferEdit {
            buffer_range: range,
            text_range: text_range_start..buffer.texts.len(),
        });
    }

    pub fn dispose(&mut self, buffer_handle: BufferHandle) {
        if let Some(buffer) = self.buffers.get_mut(buffer_handle.0 as usize) {
            buffer.dispose();
        }
    }

    pub fn iter_pending_mut<'a>(
        &'a mut self,
    ) -> impl 'a + Iterator<Item = (BufferHandle, &'a mut VersionedBuffer)> {
        self.buffers
            .iter_mut()
            .enumerate()
            .filter(|(_, e)| !e.pending_edits.is_empty())
            .map(|(i, e)| (BufferHandle(i as _), e))
    }
}

#[derive(Default)]
pub struct DiagnosticCollection {
    buffer_diagnostics: Vec<BufferDiagnosticCollection>,
}
impl DiagnosticCollection {
    pub fn buffer_diagnostics(&self, buffer_handle: BufferHandle) -> &[Diagnostic] {
        for diagnostics in &self.buffer_diagnostics {
            if diagnostics.buffer_handle == Some(buffer_handle) {
                return &diagnostics.diagnostics[..diagnostics.len];
            }
        }
        &[]
    }

    fn diagnostics_at_path(
        &mut self,
        editor: &Editor,
        root: &Path,
        path: &Path,
    ) -> &mut BufferDiagnosticCollection {
        fn find_buffer_with_path(
            editor: &Editor,
            root: &Path,
            path: &Path,
        ) -> Option<BufferHandle> {
            for buffer in editor.buffers.iter() {
                if is_editor_path_equals_to_lsp_path(
                    &editor.current_directory,
                    buffer.path(),
                    root,
                    path,
                ) {
                    return Some(buffer.handle());
                }
            }
            None
        }

        for i in 0..self.buffer_diagnostics.len() {
            if self.buffer_diagnostics[i].path == path {
                let diagnostics = &mut self.buffer_diagnostics[i];
                diagnostics.len = 0;

                if diagnostics.buffer_handle.is_none() {
                    diagnostics.buffer_handle = find_buffer_with_path(editor, root, path);
                }
                return diagnostics;
            }
        }

        let end_index = self.buffer_diagnostics.len();
        self.buffer_diagnostics.push(BufferDiagnosticCollection {
            path: path.into(),
            buffer_handle: find_buffer_with_path(editor, root, path),
            diagnostics: Vec::new(),
            len: 0,
        });
        &mut self.buffer_diagnostics[end_index]
    }

    fn clear_empty(&mut self) {
        for i in (0..self.buffer_diagnostics.len()).rev() {
            if self.buffer_diagnostics[i].len == 0 {
                self.buffer_diagnostics.swap_remove(i);
            }
        }
    }

    pub fn iter<'a>(
        &'a self,
    ) -> impl DoubleEndedIterator<Item = (&'a Path, Option<BufferHandle>, &'a [Diagnostic])> {
        self.buffer_diagnostics
            .iter()
            .map(|d| (d.path.as_path(), d.buffer_handle, &d.diagnostics[..d.len]))
    }

    pub fn on_load_buffer(&mut self, editor: &Editor, buffer_handle: BufferHandle, root: &Path) {
        let buffer_path = match editor.buffers.get(buffer_handle).map(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        for diagnostics in &mut self.buffer_diagnostics {
            if diagnostics.buffer_handle.is_none() {
                if is_editor_path_equals_to_lsp_path(
                    &editor.current_directory,
                    buffer_path,
                    root,
                    &diagnostics.path,
                ) {
                    diagnostics.buffer_handle = Some(buffer_handle);
                    return;
                }
            }
        }
    }

    pub fn on_save_buffer(&mut self, editor: &Editor, buffer_handle: BufferHandle, root: &Path) {
        let buffer_path = match editor.buffers.get(buffer_handle).map(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        for diagnostics in &mut self.buffer_diagnostics {
            if diagnostics.buffer_handle == Some(buffer_handle) {
                diagnostics.buffer_handle = None;
                if is_editor_path_equals_to_lsp_path(
                    &editor.current_directory,
                    buffer_path,
                    root,
                    &diagnostics.path,
                ) {
                    diagnostics.buffer_handle = Some(buffer_handle);
                    return;
                }
            }
        }
    }

    pub fn on_close_buffer(&mut self, buffer_handle: BufferHandle) {
        for diagnostics in &mut self.buffer_diagnostics {
            if diagnostics.buffer_handle == Some(buffer_handle) {
                diagnostics.buffer_handle = None;
                return;
            }
        }
    }
}

enum RequestState {
    Idle,
    Definition {
        client_handle: client::ClientHandle,
    },
    References {
        client_handle: client::ClientHandle,
        context_len: usize,
        auto_close_buffer: bool,
    },
    Rename {
        client_handle: client::ClientHandle,
        buffer_handle: BufferHandle,
        buffer_position: BufferPosition,
    },
    FinishRename {
        buffer_handle: BufferHandle,
        buffer_position: BufferPosition,
    },
    CodeAction {
        client_handle: client::ClientHandle,
    },
    FinishCodeAction,
    DocumentSymbols {
        client_handle: client::ClientHandle,
        buffer_view_handle: BufferViewHandle,
    },
    FinishDocumentSymbols {
        buffer_view_handle: BufferViewHandle,
    },
    WorkspaceSymbols {
        client_handle: client::ClientHandle,
        auto_close_buffer: bool,
    },
    Formatting {
        buffer_handle: BufferHandle,
    },
}
impl RequestState {
    #[inline]
    pub fn is_idle(&self) -> bool {
        matches!(self, RequestState::Idle)
    }
}

pub struct Client {
    handle: ClientHandle,
    protocol: Protocol,
    json: Json,
    root: PathBuf,
    pending_requests: PendingRequestColection,

    initialized: bool,
    server_capabilities: ServerCapabilities,
    log_write_buf: Vec<u8>,
    log_buffer_handle: Option<BufferHandle>,
    document_selectors: Vec<Glob>,
    versioned_buffers: VersionedBufferCollection,
    diagnostics: DiagnosticCollection,

    temp_edits: Vec<(BufferRange, BufferRange)>,

    request_state: RequestState,
    request_raw_json: Vec<u8>,
}

impl Client {
    fn new(handle: ClientHandle, root: PathBuf, log_buffer_handle: Option<BufferHandle>) -> Self {
        Self {
            handle,
            protocol: Protocol::new(),
            json: Json::new(),
            root,
            pending_requests: PendingRequestColection::default(),

            initialized: false,
            server_capabilities: ServerCapabilities::default(),

            log_write_buf: Vec::new(),
            log_buffer_handle,

            document_selectors: Vec::new(),
            versioned_buffers: VersionedBufferCollection::default(),
            diagnostics: DiagnosticCollection::default(),

            request_state: RequestState::Idle,
            request_raw_json: Vec::new(),
            temp_edits: Vec::new(),
        }
    }

    pub fn handle(&self) -> ClientHandle {
        self.handle
    }

    pub fn handles_path(&self, path: &[u8]) -> bool {
        if self.document_selectors.is_empty() {
            true
        } else {
            self.document_selectors.iter().any(|g| g.matches(path))
        }
    }

    pub fn diagnostics(&self) -> &DiagnosticCollection {
        &self.diagnostics
    }

    pub fn completion_triggers(&self) -> &str {
        &self.server_capabilities.completionProvider.trigger_characters
    }
    
    pub fn signature_help_triggers(&self) -> &str {
        &self.server_capabilities.signatureHelpProvider.trigger_characters
    }

    pub fn cancel_current_request(&mut self) {
        self.request_state = RequestState::Idle;
    }

    pub fn hover(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        buffer_handle: BufferHandle,
        position: BufferPosition,
    ) {
        if !self.server_capabilities.hoverProvider.0 {
            return;
        }

        let buffer_path = match editor.buffers.get(buffer_handle).map(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, editor, platform);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, &mut self.json);
        let position = DocumentPosition::from(position);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), &mut self.json);
        params.set(
            "position".into(),
            position.to_json_value(&mut self.json),
            &mut self.json,
        );

        self.request(platform, "textDocument/hover", params);
    }

    pub fn signature_help(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        buffer_handle: BufferHandle,
        position: BufferPosition,
    ) {
        if !self.server_capabilities.signatureHelpProvider.on {
            return;
        }

        let buffer_path = match editor.buffers.get(buffer_handle).map(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, editor, platform);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, &mut self.json);
        let position = DocumentPosition::from(position);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), &mut self.json);
        params.set(
            "position".into(),
            position.to_json_value(&mut self.json),
            &mut self.json,
        );

        self.request(platform, "textDocument/signatureHelp", params);
    }

    pub fn definition(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        buffer_handle: BufferHandle,
        position: BufferPosition,
        client_handle: client::ClientHandle,
    ) {
        if !self.server_capabilities.definitionProvider.0 || !self.request_state.is_idle() {
            return;
        }

        let buffer_path = match editor.buffers.get(buffer_handle).map(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, editor, platform);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, &mut self.json);
        let position = DocumentPosition::from(position);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), &mut self.json);
        params.set(
            "position".into(),
            position.to_json_value(&mut self.json),
            &mut self.json,
        );

        self.request_state = RequestState::Definition { client_handle };
        self.request(platform, "textDocument/definition", params);
    }

    pub fn references(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        buffer_handle: BufferHandle,
        position: BufferPosition,
        context_len: usize,
        auto_close_buffer: bool,
        client_handle: client::ClientHandle,
    ) {
        if !self.server_capabilities.referencesProvider.0 || !self.request_state.is_idle() {
            return;
        }

        let buffer_path = match editor.buffers.get(buffer_handle).map(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, editor, platform);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, &mut self.json);
        let position = DocumentPosition::from(position);

        let mut context = JsonObject::default();
        context.set("includeDeclaration".into(), true.into(), &mut self.json);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), &mut self.json);
        params.set(
            "position".into(),
            position.to_json_value(&mut self.json),
            &mut self.json,
        );
        params.set("context".into(), context.into(), &mut self.json);

        self.request_state = RequestState::References {
            client_handle,
            context_len,
            auto_close_buffer,
        };
        self.request(platform, "textDocument/references", params);
    }

    pub fn rename(
        &mut self,
        editor: &mut Editor,
        platform: &mut Platform,
        clients: &mut client::ClientManager,
        client_handle: client::ClientHandle,
        buffer_handle: BufferHandle,
        buffer_position: BufferPosition,
    ) {
        if !self.server_capabilities.renameProvider.on || !self.request_state.is_idle() {
            return;
        }

        let buffer_path = match editor.buffers.get(buffer_handle).map(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, editor, platform);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, &mut self.json);
        let position = DocumentPosition::from(buffer_position);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), &mut self.json);
        params.set(
            "position".into(),
            position.to_json_value(&mut self.json),
            &mut self.json,
        );

        if self.server_capabilities.renameProvider.prepare_provider {
            self.request_state = RequestState::Rename {
                client_handle,
                buffer_handle,
                buffer_position,
            };
            self.request(platform, "textDocument/prepareRename", params);
        } else {
            self.request_state = RequestState::FinishRename {
                buffer_handle,
                buffer_position,
            };
            let mut ctx = ModeContext {
                editor,
                platform,
                clients,
                client_handle,
            };
            read_line::lsp_rename::enter_mode(&mut ctx, self.handle(), "");
        }
    }

    pub fn finish_rename(&mut self, editor: &Editor, platform: &mut Platform) {
        if !self.server_capabilities.renameProvider.on {
            return;
        }
        let (buffer_handle, buffer_position) = match self.request_state {
            RequestState::FinishRename {
                buffer_handle,
                buffer_position,
            } => (buffer_handle, buffer_position),
            _ => return,
        };
        self.request_state = RequestState::Idle;

        let buffer_path = match editor.buffers.get(buffer_handle).map(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, editor, platform);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, &mut self.json);
        let position = DocumentPosition::from(buffer_position);
        let new_name = self.json.create_string(editor.read_line.input());

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), &mut self.json);
        params.set(
            "position".into(),
            position.to_json_value(&mut self.json),
            &mut self.json,
        );
        params.set("newName".into(), new_name.into(), &mut self.json);

        self.request_state = RequestState::FinishRename {
            buffer_handle,
            buffer_position,
        };
        self.request(platform, "textDocument/rename", params);
    }

    pub fn code_action(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        client_handle: client::ClientHandle,
        buffer_handle: BufferHandle,
        range: BufferRange,
    ) {
        if !self.server_capabilities.codeActionProvider.0 || !self.request_state.is_idle() {
            return;
        }

        let buffer_path = match editor.buffers.get(buffer_handle).map(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, editor, platform);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, &mut self.json);

        let mut diagnostics = JsonArray::default();
        for diagnostic in self.diagnostics.buffer_diagnostics(buffer_handle) {
            if diagnostic.range.from <= range.from && range.from < diagnostic.range.to
                || diagnostic.range.from <= range.to && range.to < diagnostic.range.to
            {
                let diagnostic = diagnostic.as_document_diagnostic(&mut self.json);
                diagnostics.push(diagnostic.to_json_value(&mut self.json), &mut self.json);
            }
        }

        let mut context = JsonObject::default();
        context.set("diagnostics".into(), diagnostics.into(), &mut self.json);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), &mut self.json);
        params.set(
            "range".into(),
            DocumentRange::from(range).to_json_value(&mut self.json),
            &mut self.json,
        );
        params.set("context".into(), context.into(), &mut self.json);

        self.request_state = RequestState::CodeAction { client_handle };
        self.request(platform, "textDocument/codeAction", params);
    }

    pub fn finish_code_action(&mut self, editor: &mut Editor, index: usize) {
        if !self.server_capabilities.codeActionProvider.0 {
            return;
        }
        match self.request_state {
            RequestState::FinishCodeAction => (),
            _ => return,
        }
        self.request_state = RequestState::Idle;

        let mut reader = io::Cursor::new(&self.request_raw_json);
        let code_actions = match self.json.read(&mut reader) {
            Ok(actions) => actions,
            Err(_) => return,
        };
        if let Some(edit) = code_actions
            .elements(&self.json)
            .filter_map(|a| DocumentCodeAction::from_json(a, &self.json).ok())
            .filter(|a| !a.disabled)
            .map(|a| a.edit)
            .nth(index)
        {
            edit.apply(editor, &mut self.temp_edits, &self.root, &self.json);
        }
    }

    pub fn document_symbols(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        client_handle: client::ClientHandle,
        buffer_view_handle: BufferViewHandle,
    ) {
        if !self.server_capabilities.documentSymbolProvider.0 || !self.request_state.is_idle() {
            return;
        }

        let buffer_path = match editor
            .buffer_views
            .get(buffer_view_handle)
            .map(|v| v.buffer_handle)
            .and_then(|h| editor.buffers.get(h))
            .map(Buffer::path)
        {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, editor, platform);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, &mut self.json);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), &mut self.json);

        self.request_state = RequestState::DocumentSymbols {
            client_handle,
            buffer_view_handle,
        };
        self.request(platform, "textDocument/documentSymbol", params);
    }

    pub fn finish_document_symbols(&mut self, editor: &mut Editor, index: usize) {
        if !self.server_capabilities.documentSymbolProvider.0 {
            return;
        }
        let buffer_view_handle = match self.request_state {
            RequestState::FinishDocumentSymbols { buffer_view_handle } => buffer_view_handle,
            _ => return,
        };
        self.request_state = RequestState::Idle;

        let buffer_view = match editor.buffer_views.get_mut(buffer_view_handle) {
            Some(buffer_view) => buffer_view,
            None => return,
        };

        let mut reader = io::Cursor::new(&self.request_raw_json);
        let symbols = match self.json.read(&mut reader) {
            Ok(symbols) => symbols,
            Err(_) => return,
        };
        if let Some(position) = symbols
            .elements(&self.json)
            .filter_map(|s| DocumentSymbolInformation::from_json(s, &self.json).ok())
            .map(|s| s.location.range.start.into())
            .nth(index)
        {
            let mut cursors = buffer_view.cursors.mut_guard();
            cursors.clear();
            cursors.add(Cursor {
                anchor: position,
                position,
            });
        }
    }

    pub fn workspace_symbols(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        client_handle: client::ClientHandle,
        query: &str,
        auto_close_buffer: bool,
    ) {
        if !self.server_capabilities.workspaceSymbolProvider.0 || !self.request_state.is_idle() {
            return;
        }

        helper::send_pending_did_change(self, editor, platform);

        let query = self.json.create_string(query);
        let mut params = JsonObject::default();
        params.set("query".into(), query.into(), &mut self.json);

        self.request_state = RequestState::WorkspaceSymbols {
            client_handle,
            auto_close_buffer,
        };
        self.request(platform, "workspace/symbol", params);
    }

    pub fn formatting(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        buffer_handle: BufferHandle,
    ) {
        if !self.server_capabilities.documentFormattingProvider.0 || !self.request_state.is_idle() {
            return;
        }

        let buffer_path = match editor.buffers.get(buffer_handle).map(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, editor, platform);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, &mut self.json);
        let mut options = JsonObject::default();
        options.set(
            "tabSize".into(),
            JsonValue::Integer(editor.config.tab_size.get() as _),
            &mut self.json,
        );
        options.set(
            "insertSpaces".into(),
            (!editor.config.indent_with_tabs).into(),
            &mut self.json,
        );
        options.set("trimTrailingWhitespace".into(), true.into(), &mut self.json);
        options.set("trimFinalNewlines".into(), true.into(), &mut self.json);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), &mut self.json);
        params.set("options".into(), options.into(), &mut self.json);

        self.request_state = RequestState::Formatting { buffer_handle };
        self.request(platform, "textDocument/formatting", params);
    }

    fn write_to_log_buffer<F>(&mut self, writer: F)
    where
        F: FnOnce(&mut Vec<u8>, &mut Json),
    {
        if let Some(_) = self.log_buffer_handle {
            writer(&mut self.log_write_buf, &mut self.json);
            self.log_write_buf.extend_from_slice(b"\n----\n\n");
        }
    }

    fn flush_log_buffer(&mut self, editor: &mut Editor) {
        let buffers = &mut editor.buffers;
        if let Some(buffer) = self.log_buffer_handle.and_then(|h| buffers.get_mut(h)) {
            let position = buffer.content().end();
            let text = String::from_utf8_lossy(&self.log_write_buf);
            buffer.insert_text(
                &mut editor.word_database,
                position,
                &text,
                &mut editor.events,
            );
            self.log_write_buf.clear();
        }
    }

    fn on_request(
        &mut self,
        editor: &mut Editor,
        platform: &mut Platform,
        clients: &mut client::ClientManager,
        request: ServerRequest,
    ) {
        macro_rules! deserialize {
            ($value:expr) => {
                match FromJson::from_json($value, &self.json) {
                    Ok(value) => value,
                    Err(_) => {
                        self.respond(platform, JsonValue::Null, Err(ResponseError::parse_error()));
                        return;
                    }
                }
            };
        }

        self.write_to_log_buffer(|buf, json| {
            use io::Write;
            let _ = write!(buf, "receive request\nid: ");
            json.write(buf, &request.id);
            let _ = write!(
                buf,
                "\nmethod: '{}'\nparams:\n",
                request.method.as_str(json)
            );
            json.write(buf, &request.params);
        });

        match request.method.as_str(&self.json) {
            "client/registerCapability" => {
                for registration in request
                    .params
                    .get("registrations", &self.json)
                    .elements(&self.json)
                {
                    declare_json_object! {
                        struct Registration {
                            method: JsonString,
                            registerOptions: JsonObject,
                        }
                    }

                    let registration: Registration = deserialize!(registration);
                    match registration.method.as_str(&self.json) {
                        "textDocument/didSave" => {
                            self.document_selectors.clear();
                            for filter in registration
                                .registerOptions
                                .get("documentSelector", &self.json)
                                .elements(&self.json)
                            {
                                declare_json_object! {
                                    struct Filter {
                                        pattern: Option<JsonString>,
                                    }
                                }
                                let filter: Filter = deserialize!(filter);
                                let pattern = match filter.pattern {
                                    Some(pattern) => pattern.as_str(&self.json),
                                    None => continue,
                                };
                                let mut glob = Glob::default();
                                if let Err(_) = glob.compile(pattern.as_bytes()) {
                                    self.document_selectors.clear();
                                    self.respond(
                                        platform,
                                        request.id,
                                        Err(ResponseError::parse_error()),
                                    );
                                    return;
                                }
                                self.document_selectors.push(glob);
                            }
                        }
                        _ => (),
                    }
                }
                self.respond(platform, request.id, Ok(JsonValue::Null));
            }
            "window/showMessage" => {
                fn parse_params(
                    params: JsonValue,
                    json: &Json,
                ) -> Result<(MessageKind, &str), JsonConvertError> {
                    let params = match params {
                        JsonValue::Object(object) => object,
                        _ => return Err(JsonConvertError),
                    };
                    let mut kind = MessageKind::Info;
                    let mut message = "";
                    for (key, value) in params.members(json) {
                        match key {
                            "type" => {
                                kind = match value {
                                    JsonValue::Integer(1) => MessageKind::Error,
                                    JsonValue::Integer(2..=4) => MessageKind::Info,
                                    _ => return Err(JsonConvertError),
                                }
                            }
                            "message" => {
                                message = match value {
                                    JsonValue::String(string) => string.as_str(json),
                                    _ => return Err(JsonConvertError),
                                }
                            }
                            _ => (),
                        }
                    }

                    Ok((kind, message))
                }

                let (kind, message) = match parse_params(request.params, &self.json) {
                    Ok(params) => params,
                    Err(_) => {
                        self.respond(platform, request.id, Err(ResponseError::parse_error()));
                        return;
                    }
                };

                editor.status_bar.write(kind).str(message);
                self.respond(platform, request.id, Ok(JsonValue::Null));
            }
            "window/showDocument" => {
                declare_json_object! {
                    struct ShowDocumentParams {
                        uri: JsonString,
                        external: Option<bool>,
                        takeFocus: Option<bool>,
                        selection: Option<DocumentRange>,
                    }
                }

                let params: ShowDocumentParams = deserialize!(request.params);
                let path = match Uri::parse(&self.root, params.uri.as_str(&self.json)) {
                    Some(Uri::Path(path)) => path,
                    None => return,
                };

                let success = if let Some(true) = params.external {
                    false
                } else {
                    let mut closure = || {
                        let client_handle = clients.focused_client()?;
                        let client = clients.get_mut(client_handle)?;
                        let buffer_view_handle =
                            editor.buffer_view_handle_from_path(client_handle, path);
                        if let Some(range) = params.selection {
                            let buffer_view = editor.buffer_views.get_mut(buffer_view_handle)?;
                            let mut cursors = buffer_view.cursors.mut_guard();
                            cursors.clear();
                            cursors.add(Cursor {
                                anchor: range.start.into(),
                                position: range.end.into(),
                            });
                        }
                        if let Some(true) = params.takeFocus {
                            client.set_buffer_view_handle(
                                Some(buffer_view_handle),
                                &mut editor.events,
                            );
                        }
                        Some(())
                    };
                    closure().is_some()
                };

                let mut result = JsonObject::default();
                result.set("success".into(), success.into(), &mut self.json);
                self.respond(platform, request.id, Ok(result.into()));
            }
            _ => self.respond(platform, request.id, Err(ResponseError::method_not_found())),
        }
    }

    fn on_notification(&mut self, editor: &mut Editor, notification: ServerNotification) {
        macro_rules! deserialize {
            ($value:expr) => {
                match FromJson::from_json($value, &self.json) {
                    Ok(value) => value,
                    Err(_) => return,
                }
            };
        }

        self.write_to_log_buffer(|buf, json| {
            use io::Write;
            let _ = write!(
                buf,
                "receive notification\nmethod: '{}'\nparams:\n",
                notification.method.as_str(json)
            );
            json.write(buf, &notification.params);
        });

        match notification.method.as_str(&self.json) {
            "window/showMessage" => {
                let mut message_type: JsonInteger = 0;
                let mut message = JsonString::default();
                for (key, value) in notification.params.members(&self.json) {
                    match key {
                        "type" => message_type = deserialize!(value),
                        "value" => message = deserialize!(value),
                        _ => (),
                    }
                }
                let message = message.as_str(&self.json);
                match message_type {
                    1 => editor.status_bar.write(MessageKind::Error).str(message),
                    2 => editor
                        .status_bar
                        .write(MessageKind::Info)
                        .fmt(format_args!("warning: {}", message)),
                    3 => editor
                        .status_bar
                        .write(MessageKind::Info)
                        .fmt(format_args!("info: {}", message)),
                    4 => editor.status_bar.write(MessageKind::Info).str(message),
                    _ => (),
                }
            }
            "textDocument/publishDiagnostics" => {
                declare_json_object! {
                    struct Params {
                        uri: JsonString,
                        diagnostics: JsonArray,
                    }
                }

                let params: Params = deserialize!(notification.params);
                let uri = params.uri.as_str(&self.json);
                let path = match Uri::parse(&self.root, uri) {
                    Some(Uri::Path(path)) => path,
                    None => return,
                };

                let diagnostics = self
                    .diagnostics
                    .diagnostics_at_path(editor, &self.root, path);
                for diagnostic in params.diagnostics.elements(&self.json) {
                    let diagnostic = deserialize!(diagnostic);
                    diagnostics.add(diagnostic, &self.json);
                }
                diagnostics.sort();
                self.diagnostics.clear_empty();
            }
            _ => (),
        }
    }

    fn on_response(
        &mut self,
        editor: &mut Editor,
        platform: &mut Platform,
        clients: &mut client::ClientManager,
        response: ServerResponse,
    ) {
        let method = match self.pending_requests.take(response.id) {
            Some(method) => method,
            None => return,
        };

        macro_rules! deserialize {
            ($value:expr) => {
                match FromJson::from_json($value, &self.json) {
                    Ok(value) => value,
                    Err(_) => return,
                }
            };
        }

        self.write_to_log_buffer(|buf, json| {
            use io::Write;
            let _ = write!(
                buf,
                "receive response\nid: {}\nmethod: '{}'\n",
                response.id.0, method
            );
            match &response.result {
                Ok(result) => {
                    let _ = write!(buf, "result:\n");
                    json.write(buf, result);
                }
                Err(error) => {
                    let _ = write!(
                        buf,
                        "error_code: {}\nerror_message: '{}'\nerror_data:\n",
                        error.code,
                        error.message.as_str(json)
                    );
                    json.write(buf, &error.data);
                }
            }
        });

        let result = match response.result {
            Ok(result) => result,
            Err(error) => {
                helper::write_response_error(&mut editor.status_bar, error, &self.json);
                return;
            }
        };

        match method {
            "initialize" => {
                self.server_capabilities = deserialize!(result.get("capabilities", &self.json));
                self.initialized = true;
                self.notify(platform, "initialized", JsonObject::default());

                for buffer in editor.buffers.iter() {
                    helper::send_did_open(self, editor, platform, buffer.handle());
                }
            }
            "textDocument/hover" => {
                let contents = result.get("contents".into(), &self.json);
                let info = helper::extract_markup_content(contents, &self.json);
                editor.status_bar.write(MessageKind::Info).str(info);
            }
            "textDocument/signatureHelp" => {
                declare_json_object! {
                    struct SignatureHelp {
                        activeSignature: usize,
                        signatures: JsonArray,
                    }
                }
                declare_json_object! {
                    struct SignatureInformation {
                        label: JsonString,
                        documentation: JsonValue,
                    }
                }

                let signature_help: Option<SignatureHelp> = deserialize!(result);
                let signature = match signature_help
                    .and_then(|sh| sh.signatures.elements(&self.json).nth(sh.activeSignature))
                {
                    Some(signature) => signature,
                    None => return,
                };
                let signature: SignatureInformation = deserialize!(signature);
                let label = signature.label.as_str(&self.json);
                let documentation =
                    helper::extract_markup_content(signature.documentation, &self.json);

                if documentation.is_empty() {
                    editor.status_bar.write(MessageKind::Info).str(label);
                } else {
                    editor
                        .status_bar
                        .write(MessageKind::Info)
                        .fmt(format_args!("{}\n{}", documentation, label));
                }
            }
            "textDocument/definition" => {
                let client_handle = match self.request_state {
                    RequestState::Definition { client_handle } => client_handle,
                    _ => return,
                };
                self.request_state = RequestState::Idle;
                let location = match result {
                    JsonValue::Object(_) => result,
                    // TODO: use picker in this case?
                    JsonValue::Array(locations) => match locations.elements(&self.json).next() {
                        Some(location) => location,
                        None => return,
                    },
                    _ => return,
                };
                let location = match DocumentLocation::from_json(location, &self.json) {
                    Ok(location) => location,
                    Err(_) => return,
                };

                let client = match clients.get_mut(client_handle) {
                    Some(client) => client,
                    None => return,
                };
                let path = match Uri::parse(&self.root, location.uri.as_str(&self.json)) {
                    Some(Uri::Path(path)) => path,
                    None => return,
                };
                let buffer_view_handle = editor.buffer_view_handle_from_path(client.handle(), path);
                if let Some(buffer_view) = editor.buffer_views.get_mut(buffer_view_handle) {
                    let position = location.range.start.into();
                    let mut cursors = buffer_view.cursors.mut_guard();
                    cursors.clear();
                    cursors.add(Cursor {
                        anchor: position,
                        position,
                    });
                }
                client.set_buffer_view_handle(Some(buffer_view_handle), &mut editor.events);
            }
            "textDocument/references" => {
                let (client_handle, auto_close_buffer, context_len) = match self.request_state {
                    RequestState::References {
                        client_handle,
                        auto_close_buffer,
                        context_len,
                    } => (client_handle, auto_close_buffer, context_len),
                    _ => return,
                };
                self.request_state = RequestState::Idle;
                let locations = match result {
                    JsonValue::Array(locations) => locations,
                    _ => return,
                };

                let client = match clients.get_mut(client_handle) {
                    Some(client) => client,
                    None => return,
                };

                let mut buffer_name = editor.string_pool.acquire();
                for location in locations.clone().elements(&self.json) {
                    let location = match DocumentLocation::from_json(location, &self.json) {
                        Ok(location) => location,
                        Err(_) => continue,
                    };
                    let path = match Uri::parse(&self.root, location.uri.as_str(&self.json)) {
                        Some(Uri::Path(path)) => path,
                        None => continue,
                    };
                    if let Some(buffer) = editor
                        .buffers
                        .find_with_path(&editor.current_directory, path)
                        .and_then(|h| editor.buffers.get(h))
                    {
                        buffer
                            .content()
                            .append_range_text_to_string(location.range.into(), &mut buffer_name);
                        break;
                    }
                }
                if buffer_name.is_empty() {
                    buffer_name.push_str("lsp");
                }
                buffer_name.push_str(".refs");

                let buffer_view_handle =
                    editor.buffer_view_handle_from_path(client.handle(), Path::new(&buffer_name));
                editor.string_pool.release(buffer_name);

                let mut context_buffer = BufferContent::new();
                let buffers = &mut editor.buffers;
                if let Some(buffer) = editor
                    .buffer_views
                    .get(buffer_view_handle)
                    .and_then(|v| buffers.get_mut(v.buffer_handle))
                {
                    buffer.capabilities = BufferCapabilities::log();
                    buffer.capabilities.auto_close = auto_close_buffer;

                    let range =
                        BufferRange::between(BufferPosition::zero(), buffer.content().end());
                    buffer.delete_range(&mut editor.word_database, range, &mut editor.events);

                    let mut text = editor.string_pool.acquire();
                    let mut last_path = "";
                    for location in locations.elements(&self.json) {
                        let location = match DocumentLocation::from_json(location, &self.json) {
                            Ok(location) => location,
                            Err(_) => continue,
                        };
                        let path = match Uri::parse(&self.root, location.uri.as_str(&self.json)) {
                            Some(Uri::Path(path)) => path,
                            None => continue,
                        };
                        let path = match path.to_str() {
                            Some(path) => path,
                            None => continue,
                        };

                        let position: BufferPosition = location.range.start.into();
                        use fmt::Write;
                        let _ = writeln!(
                            text,
                            "{}:{},{}",
                            path,
                            position.line_index + 1,
                            position.column_byte_index + 1,
                        );

                        if context_len > 0 {
                            if last_path != path {
                                context_buffer.clear();
                                if let Ok(file) = File::open(path) {
                                    let mut reader = io::BufReader::new(file);
                                    let _ = context_buffer.read(&mut reader);
                                }
                            }

                            let surrounding_len = context_len - 1;
                            let start = (location.range.start.line as usize)
                                .saturating_sub(surrounding_len);
                            let end = location.range.end.line as usize + surrounding_len;
                            let len = end - start + 1;

                            for line in context_buffer
                                .lines()
                                .skip(start)
                                .take(len)
                                .skip_while(|l| l.as_str().is_empty())
                            {
                                text.push_str(line.as_str());
                                text.push('\n');
                            }
                            text.push('\n');
                        }

                        let position = buffer.content().end();
                        buffer.insert_text(
                            &mut editor.word_database,
                            position,
                            &text,
                            &mut editor.events,
                        );
                        text.clear();

                        last_path = path;
                    }
                    editor.string_pool.release(text);
                }

                client.set_buffer_view_handle(Some(buffer_view_handle), &mut editor.events);
                editor.trigger_event_handlers(platform, clients, None);

                if let Some(buffer_view) = editor.buffer_views.get_mut(buffer_view_handle) {
                    let mut cursors = buffer_view.cursors.mut_guard();
                    cursors.clear();
                    cursors.add(Cursor {
                        anchor: BufferPosition::zero(),
                        position: BufferPosition::zero(),
                    });
                }
            }
            "textDocument/prepareRename" => {
                let (client_handle, buffer_handle, buffer_position) = match self.request_state {
                    RequestState::Rename {
                        client_handle,
                        buffer_handle,
                        buffer_position,
                    } => (client_handle, buffer_handle, buffer_position),
                    _ => return,
                };
                self.request_state = RequestState::Idle;
                let result = match result {
                    JsonValue::Null => {
                        editor
                            .status_bar
                            .write(MessageKind::Error)
                            .str("could not rename item under cursor");
                        return;
                    }
                    JsonValue::Object(result) => result,
                    _ => return,
                };
                let mut range = DocumentRange::default();
                let mut placeholder: Option<JsonString> = None;
                let mut default_behaviour: Option<bool> = None;
                for (key, value) in result.members(&self.json) {
                    match key {
                        "start" => range.start = deserialize!(value),
                        "end" => range.end = deserialize!(value),
                        "range" => range = deserialize!(value),
                        "placeholder" => placeholder = deserialize!(value),
                        "defaultBehavior" => default_behaviour = deserialize!(value),
                        _ => (),
                    }
                }

                let buffer = match editor.buffers.get(buffer_handle) {
                    Some(buffer) => buffer,
                    None => return,
                };

                let mut range = range.into();
                if let Some(true) = default_behaviour {
                    let word = buffer.content().word_at(buffer_position);
                    range = BufferRange::between(word.position, word.end_position());
                }

                let mut input = editor.string_pool.acquire();
                match placeholder {
                    Some(text) => input.push_str(text.as_str(&self.json)),
                    None => buffer
                        .content()
                        .append_range_text_to_string(range, &mut input),
                }

                let mut ctx = ModeContext {
                    editor,
                    platform,
                    clients,
                    client_handle,
                };
                read_line::lsp_rename::enter_mode(&mut ctx, self.handle(), &input);
                editor.string_pool.release(input);

                self.request_state = RequestState::FinishRename {
                    buffer_handle,
                    buffer_position,
                };
            }
            "textDocument/rename" => {
                let edit: WorkspaceEdit = deserialize!(result);
                edit.apply(editor, &mut self.temp_edits, &self.root, &self.json);
            }
            "textDocument/codeAction" => {
                let client_handle = match self.request_state {
                    RequestState::CodeAction { client_handle } => client_handle,
                    _ => return,
                };
                self.request_state = RequestState::Idle;
                let actions = match result {
                    JsonValue::Array(actions) => actions,
                    _ => return,
                };

                editor.picker.clear();
                for action in actions
                    .clone()
                    .elements(&self.json)
                    .filter_map(|a| DocumentCodeAction::from_json(a, &self.json).ok())
                    .filter(|a| !a.disabled)
                {
                    editor
                        .picker
                        .add_custom_entry(action.title.as_str(&self.json));
                }

                let mut ctx = ModeContext {
                    editor,
                    platform,
                    clients,
                    client_handle,
                };
                picker::lsp_code_action::enter_mode(&mut ctx, self.handle());

                self.request_state = RequestState::FinishCodeAction;
                self.request_raw_json.clear();
                self.json.write(&mut self.request_raw_json, &actions.into());
            }
            "textDocument/documentSymbol" => {
                let (client_handle, buffer_view_handle) = match self.request_state {
                    RequestState::DocumentSymbols {
                        client_handle,
                        buffer_view_handle,
                    } => (client_handle, buffer_view_handle),
                    _ => return,
                };
                self.request_state = RequestState::Idle;

                let symbols = match result {
                    JsonValue::Array(symbols) => symbols,
                    _ => return,
                };

                editor.picker.clear();
                for symbol in symbols
                    .clone()
                    .elements(&self.json)
                    .filter_map(|s| DocumentSymbolInformation::from_json(s, &self.json).ok())
                {
                    match symbol.container_name {
                        Some(container_name) => {
                            let name = symbol.name.as_str(&self.json);
                            let container_name = container_name.as_str(&self.json);
                            editor.picker.add_custom_entry_fmt(format_args!(
                                "{} ({})",
                                name, container_name
                            ));
                        }
                        None => {
                            editor
                                .picker
                                .add_custom_entry(symbol.name.as_str(&self.json));
                        }
                    }
                }

                let mut ctx = ModeContext {
                    editor,
                    platform,
                    clients,
                    client_handle,
                };
                picker::lsp_document_symbol::enter_mode(&mut ctx, self.handle());

                self.request_state = RequestState::FinishDocumentSymbols { buffer_view_handle };
                self.request_raw_json.clear();
                self.json.write(&mut self.request_raw_json, &symbols.into());
            }
            "workspace/symbol" => {
                let (client_handle, auto_close_buffer) = match self.request_state {
                    RequestState::WorkspaceSymbols {
                        client_handle,
                        auto_close_buffer,
                    } => (client_handle, auto_close_buffer),
                    _ => return,
                };
                self.request_state = RequestState::Idle;
                let symbols = match result {
                    JsonValue::Array(symbols) => symbols,
                    _ => return,
                };

                let client = match clients.get_mut(client_handle) {
                    Some(client) => client,
                    None => return,
                };

                let buffer_view_handle = editor.buffer_view_handle_from_path(
                    client.handle(),
                    Path::new("workspace-symbols.refs"),
                );

                let buffers = &mut editor.buffers;
                if let Some(buffer) = editor
                    .buffer_views
                    .get(buffer_view_handle)
                    .and_then(|v| buffers.get_mut(v.buffer_handle))
                {
                    buffer.capabilities = BufferCapabilities::log();
                    buffer.capabilities.auto_close = auto_close_buffer;

                    let range =
                        BufferRange::between(BufferPosition::zero(), buffer.content().end());
                    buffer.delete_range(&mut editor.word_database, range, &mut editor.events);

                    let mut count = 0;
                    let mut text = editor.string_pool.acquire();
                    for symbol in symbols.elements(&self.json) {
                        count += 1;
                        let symbol = match DocumentSymbolInformation::from_json(symbol, &self.json)
                        {
                            Ok(symbol) => symbol,
                            Err(_) => continue,
                        };
                        let path =
                            match Uri::parse(&self.root, symbol.location.uri.as_str(&self.json)) {
                                Some(Uri::Path(path)) => path,
                                None => continue,
                            };
                        let path = match path.to_str() {
                            Some(path) => path,
                            None => continue,
                        };

                        let position: BufferPosition = symbol.location.range.start.into();
                        use fmt::Write;
                        let _ = write!(
                            text,
                            "{}:{},{}:",
                            path,
                            position.line_index + 1,
                            position.column_byte_index + 1,
                        );
                        text.push_str(symbol.name.as_str(&self.json));
                        if let Some(container_name) = symbol.container_name {
                            text.push_str(" (");
                            text.push_str(container_name.as_str(&self.json));
                            text.push(')');
                        }
                        text.push('\n');

                        let position = buffer.content().end();
                        buffer.insert_text(
                            &mut editor.word_database,
                            position,
                            &text,
                            &mut editor.events,
                        );
                        text.clear();
                    }
                    editor.string_pool.release(text);

                    editor
                        .status_bar
                        .write(MessageKind::Info)
                        .fmt(format_args!("symbol count: {}", count));
                }

                client.set_buffer_view_handle(Some(buffer_view_handle), &mut editor.events);
                editor.trigger_event_handlers(platform, clients, None);

                if let Some(buffer_view) = editor.buffer_views.get_mut(buffer_view_handle) {
                    let mut cursors = buffer_view.cursors.mut_guard();
                    cursors.clear();
                    cursors.add(Cursor {
                        anchor: BufferPosition::zero(),
                        position: BufferPosition::zero(),
                    });
                }
            }
            "textDocument/formatting" => {
                let buffer_handle = match self.request_state {
                    RequestState::Formatting { buffer_handle } => buffer_handle,
                    _ => return,
                };
                self.request_state = RequestState::Idle;
                let edits = match result {
                    JsonValue::Array(edits) => edits,
                    _ => return,
                };
                TextEdit::apply_edits(
                    editor,
                    buffer_handle,
                    &mut self.temp_edits,
                    edits,
                    &self.json,
                );
            }
            _ => (),
        }
    }

    fn on_parse_error(&mut self, platform: &mut Platform, request_id: JsonValue) {
        self.write_to_log_buffer(|buf, json| {
            use io::Write;
            let _ = write!(buf, "send parse error\nrequest_id: ");
            json.write(buf, &request_id);
        });
        self.respond(platform, request_id, Err(ResponseError::parse_error()))
    }

    fn on_editor_events(&mut self, editor: &Editor, platform: &mut Platform) {
        if !self.initialized {
            return;
        }

        let mut events = EditorEventIter::new();
        while let Some(event) = events.next(&editor.events) {
            match event {
                &EditorEvent::Idle => {
                    helper::send_pending_did_change(self, editor, platform);
                }
                &EditorEvent::BufferLoad { handle } => {
                    let handle = handle;
                    self.versioned_buffers.dispose(handle);
                    self.diagnostics.on_load_buffer(editor, handle, &self.root);
                    helper::send_did_open(self, editor, platform, handle);
                }
                &EditorEvent::BufferInsertText {
                    handle,
                    range,
                    text,
                } => {
                    let text = text.as_str(&editor.events);
                    let range = BufferRange::between(range.from, range.from);
                    self.versioned_buffers.add_edit(handle, range, text);
                }
                &EditorEvent::BufferDeleteText { handle, range } => {
                    self.versioned_buffers.add_edit(handle, range, "");
                }
                &EditorEvent::BufferSave { handle, .. } => {
                    self.diagnostics.on_save_buffer(editor, handle, &self.root);
                    helper::send_pending_did_change(self, editor, platform);
                    helper::send_did_save(self, editor, platform, handle);
                }
                &EditorEvent::BufferClose { handle } => {
                    if self.log_buffer_handle == Some(handle) {
                        self.log_buffer_handle = None;
                    }
                    self.versioned_buffers.dispose(handle);
                    self.diagnostics.on_close_buffer(handle);
                    helper::send_pending_did_change(self, editor, platform);
                    helper::send_did_close(self, editor, platform, handle);
                }
                EditorEvent::ClientChangeBufferView { .. } => (),
            }
        }
    }

    fn request(&mut self, platform: &mut Platform, method: &'static str, params: JsonObject) {
        if !self.initialized {
            return;
        }

        let params = params.into();
        self.write_to_log_buffer(|buf, json| {
            use io::Write;
            let _ = write!(buf, "send request\nmethod: '{}'\nparams:\n", method);
            json.write(buf, &params);
        });
        let id = self
            .protocol
            .request(platform, &mut self.json, method, params);
        self.pending_requests.add(id, method);
    }

    fn respond(
        &mut self,
        platform: &mut Platform,
        request_id: JsonValue,
        result: Result<JsonValue, ResponseError>,
    ) {
        self.write_to_log_buffer(|buf, json| {
            use io::Write;
            let _ = write!(buf, "send response\nid: ");
            json.write(buf, &request_id);
            match &result {
                Ok(result) => {
                    let _ = write!(buf, "\nresult:\n");
                    json.write(buf, result);
                }
                Err(error) => {
                    let _ = write!(
                        buf,
                        "\nerror.code: {}\nerror.message: {}\nerror.data:\n",
                        error.code,
                        error.message.as_str(json)
                    );
                    json.write(buf, &error.data);
                }
            }
        });
        self.protocol
            .respond(platform, &mut self.json, request_id, result);
    }

    fn notify(&mut self, platform: &mut Platform, method: &'static str, params: JsonObject) {
        let params = params.into();
        self.write_to_log_buffer(|buf, json| {
            use io::Write;
            let _ = write!(buf, "send notification\nmethod: '{}'\nparams:\n", method);
            json.write(buf, &params);
        });
        self.protocol
            .notify(platform, &mut self.json, method, params);
    }

    fn initialize(&mut self, platform: &mut Platform) {
        let mut params = JsonObject::default();
        params.set(
            "processId".into(),
            JsonValue::Integer(process::id() as _),
            &mut self.json,
        );

        let mut client_info = JsonObject::default();
        client_info.set("name".into(), env!("CARGO_PKG_NAME").into(), &mut self.json);
        client_info.set(
            "name".into(),
            env!("CARGO_PKG_VERSION").into(),
            &mut self.json,
        );
        params.set("clientInfo".into(), client_info.into(), &mut self.json);

        let root = self
            .json
            .fmt_string(format_args!("{}", Uri::Path(&self.root)));
        params.set("rootUri".into(), root.into(), &mut self.json);

        params.set(
            "capabilities".into(),
            capabilities::client_capabilities(&mut self.json),
            &mut self.json,
        );

        self.initialized = true;
        self.request(platform, "initialize", params);
        self.initialized = false;
    }
}

mod helper {
    use super::*;

    pub fn write_response_error(status_bar: &mut StatusBar, error: ResponseError, json: &Json) {
        status_bar
            .write(MessageKind::Error)
            .str(error.message.as_str(json));
    }

    pub fn text_document_with_id(root: &Path, path: &Path, json: &mut Json) -> JsonObject {
        let uri = if path.is_absolute() {
            json.fmt_string(format_args!("{}", Uri::Path(path)))
        } else {
            match path.to_str() {
                Some(path) => json.fmt_string(format_args!("{}/{}", Uri::Path(root), path)),
                None => return JsonObject::default(),
            }
        };
        let mut id = JsonObject::default();
        id.set("uri".into(), uri.into(), json);
        id
    }

    pub fn extract_markup_content<'json>(content: JsonValue, json: &'json Json) -> &'json str {
        match content {
            JsonValue::String(s) => s.as_str(json),
            JsonValue::Object(o) => match o.get("value".into(), json) {
                JsonValue::String(s) => s.as_str(json),
                _ => "",
            },
            _ => "",
        }
    }

    pub fn send_did_open(
        client: &mut Client,
        editor: &Editor,
        platform: &mut Platform,
        buffer_handle: BufferHandle,
    ) {
        if !client.server_capabilities.textDocumentSync.open_close {
            return;
        }

        let buffer = match editor.buffers.get(buffer_handle) {
            Some(buffer) => buffer,
            None => return,
        };
        if !buffer.capabilities.can_save {
            return;
        }

        let buffer_path = buffer.path();
        let mut text_document = text_document_with_id(&client.root, buffer_path, &mut client.json);
        let language_id = client
            .json
            .create_string(protocol::path_to_language_id(buffer_path));
        text_document.set("languageId".into(), language_id.into(), &mut client.json);
        text_document.set("version".into(), JsonValue::Integer(0), &mut client.json);
        let text = client.json.fmt_string(format_args!("{}", buffer.content()));
        text_document.set("text".into(), text.into(), &mut client.json);

        let mut params = JsonObject::default();
        params.set(
            "textDocument".into(),
            text_document.into(),
            &mut client.json,
        );

        client.notify(platform, "textDocument/didOpen", params.into());
    }

    pub fn send_pending_did_change(client: &mut Client, editor: &Editor, platform: &mut Platform) {
        if let TextDocumentSyncKind::None = client.server_capabilities.textDocumentSync.change {
            return;
        }

        let mut versioned_buffers = std::mem::take(&mut client.versioned_buffers);
        for (buffer_handle, versioned_buffer) in versioned_buffers.iter_pending_mut() {
            let buffer = match editor.buffers.get(buffer_handle) {
                Some(buffer) => buffer,
                None => continue,
            };
            if !buffer.capabilities.can_save {
                continue;
            }

            let mut text_document =
                text_document_with_id(&client.root, buffer.path(), &mut client.json);
            text_document.set(
                "version".into(),
                JsonValue::Integer(versioned_buffer.version as _),
                &mut client.json,
            );

            let mut params = JsonObject::default();
            params.set(
                "textDocument".into(),
                text_document.into(),
                &mut client.json,
            );

            let mut content_changes = JsonArray::default();
            match client.server_capabilities.textDocumentSync.save {
                TextDocumentSyncKind::None => (),
                TextDocumentSyncKind::Full => {
                    let text = client.json.fmt_string(format_args!("{}", buffer.content()));
                    let mut change_event = JsonObject::default();
                    change_event.set("text".into(), text.into(), &mut client.json);
                    content_changes.push(change_event.into(), &mut client.json);
                }
                TextDocumentSyncKind::Incremental => {
                    for edit in &versioned_buffer.pending_edits {
                        let mut change_event = JsonObject::default();

                        let edit_range =
                            DocumentRange::from(edit.buffer_range).to_json_value(&mut client.json);
                        change_event.set("range".into(), edit_range, &mut client.json);

                        let text = &versioned_buffer.texts[edit.text_range.clone()];
                        let text = client.json.create_string(text);
                        change_event.set("text".into(), text.into(), &mut client.json);

                        content_changes.push(change_event.into(), &mut client.json);
                    }
                }
            }

            params.set(
                "contentChanges".into(),
                content_changes.into(),
                &mut client.json,
            );

            versioned_buffer.flush();
            client.notify(platform, "textDocument/didChange", params.into());
        }
        std::mem::swap(&mut client.versioned_buffers, &mut versioned_buffers);
    }

    pub fn send_did_save(
        client: &mut Client,
        editor: &Editor,
        platform: &mut Platform,
        buffer_handle: BufferHandle,
    ) {
        if let TextDocumentSyncKind::None = client.server_capabilities.textDocumentSync.save {
            return;
        }

        let buffer = match editor.buffers.get(buffer_handle) {
            Some(buffer) => buffer,
            None => return,
        };
        if !buffer.capabilities.can_save {
            return;
        }

        let text_document = text_document_with_id(&client.root, buffer.path(), &mut client.json);
        let mut params = JsonObject::default();
        params.set(
            "textDocument".into(),
            text_document.into(),
            &mut client.json,
        );

        if let TextDocumentSyncKind::Full = client.server_capabilities.textDocumentSync.save {
            let text = client.json.fmt_string(format_args!("{}", buffer.content()));
            params.set("text".into(), text.into(), &mut client.json);
        }

        client.notify(platform, "textDocument/didSave", params.into())
    }

    pub fn send_did_close(
        client: &mut Client,
        editor: &Editor,
        platform: &mut Platform,
        buffer_handle: BufferHandle,
    ) {
        if !client.server_capabilities.textDocumentSync.open_close {
            return;
        }

        let buffer = match editor.buffers.get(buffer_handle) {
            Some(buffer) => buffer,
            None => return,
        };
        if !buffer.capabilities.can_save {
            return;
        }

        let text_document = text_document_with_id(&client.root, buffer.path(), &mut client.json);
        let mut params = JsonObject::default();
        params.set(
            "textDocument".into(),
            text_document.into(),
            &mut client.json,
        );

        client.notify(platform, "textDocument/didClose", params.into());
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ClientHandle(u8);
impl fmt::Display for ClientHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for ClientHandle {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse() {
            Ok(i) => Ok(Self(i)),
            Err(_) => Err(()),
        }
    }
}

struct ClientRecipe {
    glob: Glob,
    command: String,
    environment: String,
    root: PathBuf,
    log_buffer_name: String,
    running_client: Option<ClientHandle>,
}

enum ClientEntry {
    Vacant,
    Reserved,
    Occupied(Client),
}
impl ClientEntry {
    pub fn reserve_and_take(&mut self) -> Option<Client> {
        let mut entry = ClientEntry::Reserved;
        std::mem::swap(self, &mut entry);
        match entry {
            ClientEntry::Occupied(client) => Some(client),
            _ => None,
        }
    }
}

pub struct ClientManager {
    entries: Vec<ClientEntry>,
    recipes: Vec<ClientRecipe>,
}

impl ClientManager {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            recipes: Vec::new(),
        }
    }

    pub fn add_recipe(
        &mut self,
        glob: &[u8],
        command: &str,
        environment: &str,
        root: Option<&Path>,
        log_buffer_name: Option<&str>,
    ) -> Result<(), InvalidGlobError> {
        for recipe in &mut self.recipes {
            if recipe.command == command {
                recipe.glob.compile(glob)?;
                recipe.environment.clear();
                recipe.environment.push_str(environment);
                recipe.root.clear();
                if let Some(path) = root {
                    recipe.root.push(path);
                }
                recipe.log_buffer_name.clear();
                if let Some(name) = log_buffer_name {
                    recipe.log_buffer_name.push_str(name);
                }
                recipe.running_client = None;
                return Ok(());
            }
        }

        let mut recipe_glob = Glob::default();
        recipe_glob.compile(glob)?;
        self.recipes.push(ClientRecipe {
            glob: recipe_glob,
            command: command.into(),
            environment: environment.into(),
            root: root.unwrap_or(Path::new("")).into(),
            log_buffer_name: log_buffer_name.unwrap_or("").into(),
            running_client: None,
        });
        Ok(())
    }

    pub fn start(
        &mut self,
        platform: &mut Platform,
        mut command: Command,
        root: PathBuf,
        log_buffer_handle: Option<BufferHandle>,
    ) -> ClientHandle {
        fn find_free_slot(this: &mut ClientManager) -> ClientHandle {
            for (i, slot) in this.entries.iter_mut().enumerate() {
                if let ClientEntry::Vacant = slot {
                    *slot = ClientEntry::Reserved;
                    return ClientHandle(i as _);
                }
            }
            let handle = ClientHandle(this.entries.len() as _);
            this.entries.push(ClientEntry::Reserved);
            handle
        }

        let handle = find_free_slot(self);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        platform.enqueue_request(PlatformRequest::SpawnProcess {
            tag: ProcessTag::Lsp(handle),
            command,
            buf_len: protocol::BUFFER_LEN,
        });
        self.entries[handle.0 as usize] =
            ClientEntry::Occupied(Client::new(handle, root, log_buffer_handle));
        handle
    }

    pub fn stop(&mut self, platform: &mut Platform, handle: ClientHandle) {
        if let ClientEntry::Occupied(client) = &mut self.entries[handle.0 as usize] {
            let _ = client.notify(platform, "exit", JsonObject::default());
            self.entries[handle.0 as usize] = ClientEntry::Vacant;

            for recipe in &mut self.recipes {
                if recipe.running_client == Some(handle) {
                    recipe.running_client = None;
                }
            }
        }
    }

    pub fn stop_all(&mut self, platform: &mut Platform) {
        for i in 0..self.entries.len() {
            self.stop(platform, ClientHandle(i as _));
        }
    }

    pub fn get(&self, handle: ClientHandle) -> Option<&Client> {
        match self.entries[handle.0 as usize] {
            ClientEntry::Occupied(ref client) => Some(client),
            _ => None,
        }
    }

    pub fn access<A, R>(editor: &mut Editor, handle: ClientHandle, accessor: A) -> Option<R>
    where
        A: FnOnce(&mut Editor, &mut Client) -> R,
    {
        let mut client = editor.lsp.entries[handle.0 as usize].reserve_and_take()?;
        let result = accessor(editor, &mut client);
        editor.lsp.entries[handle.0 as usize] = ClientEntry::Occupied(client);
        Some(result)
    }

    pub fn clients(&self) -> impl DoubleEndedIterator<Item = &Client> {
        self.entries.iter().flat_map(|e| match e {
            ClientEntry::Occupied(client) => Some(client),
            _ => None,
        })
    }

    pub fn on_process_spawned(
        editor: &mut Editor,
        platform: &mut Platform,
        handle: ClientHandle,
        process_handle: ProcessHandle,
    ) {
        if let ClientEntry::Occupied(ref mut client) = editor.lsp.entries[handle.0 as usize] {
            client.protocol.set_process_handle(process_handle);
            client.initialize(platform);
        }
    }

    pub fn on_process_output(
        editor: &mut Editor,
        platform: &mut Platform,
        clients: &mut client::ClientManager,
        handle: ClientHandle,
        bytes: &[u8],
    ) {
        let mut client = match editor.lsp.entries[handle.0 as usize].reserve_and_take() {
            Some(client) => client,
            None => return,
        };

        let mut events = client.protocol.parse_events(bytes);
        while let Some(event) = events.next(&mut client.protocol, &mut client.json) {
            match event {
                ServerEvent::Closed => editor.lsp.stop(platform, handle),
                ServerEvent::ParseError => client.on_parse_error(platform, JsonValue::Null),
                ServerEvent::Request(request) => {
                    client.on_request(editor, platform, clients, request)
                }
                ServerEvent::Notification(notification) => {
                    client.on_notification(editor, notification)
                }
                ServerEvent::Response(response) => {
                    client.on_response(editor, platform, clients, response)
                }
            }
            client.flush_log_buffer(editor);
        }
        events.finish(&mut client.protocol);

        editor.lsp.entries[handle.0 as usize] = ClientEntry::Occupied(client);
    }

    pub fn on_process_exit(editor: &mut Editor, handle: ClientHandle) {
        editor.lsp.entries[handle.0 as usize] = ClientEntry::Vacant;
        for recipe in &mut editor.lsp.recipes {
            if recipe.running_client == Some(handle) {
                recipe.running_client = None;
            }
        }
    }

    pub fn on_editor_events(editor: &mut Editor, platform: &mut Platform) {
        let mut events = EditorEventIter::new();
        while let Some(event) = events.next(&editor.events) {
            if let &EditorEvent::BufferLoad { handle } = event {
                let buffer_path = match editor
                    .buffers
                    .get(handle)
                    .map(Buffer::path)
                    .and_then(Path::to_str)
                {
                    Some(path) => path,
                    None => continue,
                };
                let (index, recipe) = match editor
                    .lsp
                    .recipes
                    .iter_mut()
                    .enumerate()
                    .find(|(_, r)| r.glob.matches(buffer_path.as_bytes()))
                {
                    Some(recipe) => recipe,
                    None => continue,
                };
                if recipe.running_client.is_some() {
                    continue;
                }
                let command = match parse_process_command(&recipe.command, &recipe.environment) {
                    Ok(command) => command,
                    Err(error) => {
                        let error =
                            error.display(&recipe.command, None, &editor.commands, &editor.buffers);
                        editor
                            .status_bar
                            .write(MessageKind::Error)
                            .fmt(format_args!("{}", error));
                        continue;
                    }
                };
                let root = if recipe.root.as_os_str().is_empty() {
                    editor.current_directory.clone()
                } else {
                    recipe.root.clone()
                };

                let log_buffer_handle = if !recipe.log_buffer_name.is_empty() {
                    let mut buffer = editor.buffers.add_new();
                    buffer.capabilities = BufferCapabilities::log();
                    buffer.set_path(Path::new(&recipe.log_buffer_name));
                    Some(buffer.handle())
                } else {
                    None
                };

                let client_handle = editor.lsp.start(platform, command, root, log_buffer_handle);
                editor.lsp.recipes[index].running_client = Some(client_handle);
            }
        }

        for i in 0..editor.lsp.entries.len() {
            if let Some(mut client) = editor.lsp.entries[i].reserve_and_take() {
                client.on_editor_events(editor, platform);
                editor.lsp.entries[i] = ClientEntry::Occupied(client);
            }
        }
    }
}
