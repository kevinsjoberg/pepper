use std::path::Path;

use pepper::{
    buffer::{parse_path_and_position, BufferProperties},
    buffer_position::BufferPosition,
    client::ClientHandle,
    cursor::Cursor,
    editor::{Editor, EditorContext, EditorControlFlow, KeysIterator},
    editor_utils::{MessageKind, ReadLinePoll},
    mode::ModeKind,
    picker::EntrySource,
    plugin::PluginHandle,
    word_database::WordIndicesIter,
};

use crate::{
    client::{Client, ClientOperation},
    LspPlugin,
};

pub fn enter_definition_mode(editor: &mut Editor, plugin_handle: PluginHandle) -> ClientOperation {
    fn on_client_keys(
        ctx: &mut EditorContext,
        client_handle: ClientHandle,
        _: &mut KeysIterator,
        poll: ReadLinePoll,
    ) -> Option<EditorControlFlow> {
        match poll {
            ReadLinePoll::Pending => Some(EditorControlFlow::Continue),
            ReadLinePoll::Submitted => {
                if let Some((_, entry)) = ctx.editor.picker.current_entry(&ctx.editor.word_database)
                {
                    let (path, position) = parse_path_and_position(entry);
                    let position = match position {
                        Some(position) => position,
                        None => BufferPosition::zero(),
                    };

                    let path = ctx.editor.string_pool.acquire_with(path);
                    match ctx.editor.buffer_view_handle_from_path(
                        client_handle,
                        Path::new(&path),
                        BufferProperties::text(),
                        false,
                    ) {
                        Ok(buffer_view_handle) => {
                            let client = ctx.clients.get_mut(client_handle);
                            client.set_buffer_view_handle(
                                Some(buffer_view_handle),
                                &ctx.editor.buffer_views,
                                &mut ctx.editor.events,
                            );

                            let mut cursors = ctx
                                .editor
                                .buffer_views
                                .get_mut(buffer_view_handle)
                                .cursors
                                .mut_guard();
                            cursors.clear();
                            cursors.add(Cursor {
                                anchor: position,
                                position,
                            });
                        }
                        Err(error) => ctx
                            .editor
                            .status_bar
                            .write(MessageKind::Error)
                            .fmt(format_args!("{}", error)),
                    }
                    ctx.editor.string_pool.release(path);
                }
                ctx.editor.enter_mode(ModeKind::default());
                Some(EditorControlFlow::Continue)
            }
            ReadLinePoll::Canceled => {
                ctx.editor.enter_mode(ModeKind::default());
                Some(EditorControlFlow::Continue)
            }
        }
    }

    editor.read_line.set_prompt("definition:");
    editor.picker.filter(WordIndicesIter::empty(), "");
    editor.picker.move_cursor(0);

    if editor.picker.len() > 0 {
        let state = &mut editor.mode.picker_state;
        state.on_client_keys = on_client_keys;
        state.plugin_handle = Some(plugin_handle);
        editor.enter_mode(ModeKind::Picker);
    }

    ClientOperation::EnteredPickerMode
}

pub fn enter_code_action_mode(
    editor: &mut Editor,
    plugin_handle: PluginHandle,
    client: &mut Client,
) -> ClientOperation {
    fn on_client_keys(
        ctx: &mut EditorContext,
        _: ClientHandle,
        _: &mut KeysIterator,
        poll: ReadLinePoll,
    ) -> Option<EditorControlFlow> {
        match poll {
            ReadLinePoll::Pending => Some(EditorControlFlow::Continue),
            ReadLinePoll::Submitted => {
                if let Some(handle) = ctx.editor.mode.picker_state.plugin_handle {
                    let lsp = ctx.plugins.get_as::<LspPlugin>(handle);
                    if let Some(client) =
                        lsp.picker_client_handle.take().and_then(|h| lsp.get_mut(h))
                    {
                        let index = match ctx.editor.picker.current_entry(&ctx.editor.word_database)
                        {
                            Some((EntrySource::Custom(i), _)) => i,
                            _ => 0,
                        };
                        client.finish_code_action(&mut ctx.editor, index);
                    }
                }

                ctx.editor.enter_mode(ModeKind::default());
                Some(EditorControlFlow::Continue)
            }
            ReadLinePoll::Canceled => {
                if let Some(handle) = ctx.editor.mode.picker_state.plugin_handle {
                    let lsp = ctx.plugins.get_as::<LspPlugin>(handle);
                    if let Some(client) =
                        lsp.picker_client_handle.take().and_then(|h| lsp.get_mut(h))
                    {
                        client.cancel_current_request();
                    }
                }

                ctx.editor.enter_mode(ModeKind::default());
                Some(EditorControlFlow::Continue)
            }
        }
    }

    editor.read_line.set_prompt("code action:");
    editor.picker.filter(WordIndicesIter::empty(), "");
    editor.picker.move_cursor(0);

    if editor.picker.len() > 0 {
        let state = &mut editor.mode.picker_state;
        state.on_client_keys = on_client_keys;
        state.plugin_handle = Some(plugin_handle);
        editor.enter_mode(ModeKind::Picker);
        ClientOperation::EnteredPickerMode
    } else {
        client.cancel_current_request();
        ClientOperation::None
    }
}

pub fn enter_document_symbol_mode(
    editor: &mut Editor,
    plugin_handle: PluginHandle,
    client: &mut Client,
) -> ClientOperation {
    fn on_client_keys(
        ctx: &mut EditorContext,
        client_handle: ClientHandle,
        _: &mut KeysIterator,
        poll: ReadLinePoll,
    ) -> Option<EditorControlFlow> {
        match poll {
            ReadLinePoll::Pending => Some(EditorControlFlow::Continue),
            ReadLinePoll::Submitted => {
                if let Some(handle) = ctx.editor.mode.picker_state.plugin_handle {
                    let lsp = ctx.plugins.get_as::<LspPlugin>(handle);
                    if let Some(client) =
                        lsp.picker_client_handle.take().and_then(|h| lsp.get_mut(h))
                    {
                        let index = match ctx.editor.picker.current_entry(&ctx.editor.word_database)
                        {
                            Some((EntrySource::Custom(i), _)) => i,
                            _ => 0,
                        };
                        client.finish_document_symbols(
                            &mut ctx.editor,
                            &mut ctx.clients,
                            client_handle,
                            index,
                        );
                    }
                }

                ctx.editor.enter_mode(ModeKind::default());
                Some(EditorControlFlow::Continue)
            }
            ReadLinePoll::Canceled => {
                if let Some(handle) = ctx.editor.mode.picker_state.plugin_handle {
                    let lsp = ctx.plugins.get_as::<LspPlugin>(handle);
                    if let Some(client) =
                        lsp.picker_client_handle.take().and_then(|h| lsp.get_mut(h))
                    {
                        client.cancel_current_request();
                    }
                }

                ctx.editor.enter_mode(ModeKind::default());
                Some(EditorControlFlow::Continue)
            }
        }
    }

    editor.read_line.set_prompt("document symbol:");
    editor.picker.filter(WordIndicesIter::empty(), "");
    editor.picker.move_cursor(0);

    if editor.picker.len() > 0 {
        let state = &mut editor.mode.picker_state;
        state.on_client_keys = on_client_keys;
        state.plugin_handle = Some(plugin_handle);
        editor.enter_mode(ModeKind::Picker);
        ClientOperation::EnteredPickerMode
    } else {
        client.cancel_current_request();
        ClientOperation::None
    }
}

pub fn enter_workspace_symbol_mode(
    editor: &mut Editor,
    plugin_handle: PluginHandle,
    client: &mut Client,
) -> ClientOperation {
    fn on_client_keys(
        ctx: &mut EditorContext,
        client_handle: ClientHandle,
        _: &mut KeysIterator,
        poll: ReadLinePoll,
    ) -> Option<EditorControlFlow> {
        match poll {
            ReadLinePoll::Pending => Some(EditorControlFlow::Continue),
            ReadLinePoll::Submitted => {
                if let Some(handle) = ctx.editor.mode.picker_state.plugin_handle {
                    let lsp = ctx.plugins.get_as::<LspPlugin>(handle);
                    if let Some(client) =
                        lsp.picker_client_handle.take().and_then(|h| lsp.get_mut(h))
                    {
                        let index = match ctx.editor.picker.current_entry(&ctx.editor.word_database)
                        {
                            Some((EntrySource::Custom(i), _)) => i,
                            _ => 0,
                        };
                        client.finish_workspace_symbols(
                            &mut ctx.editor,
                            &mut ctx.clients,
                            client_handle,
                            index,
                        );
                    }
                }

                ctx.editor.enter_mode(ModeKind::default());
                Some(EditorControlFlow::Continue)
            }
            ReadLinePoll::Canceled => {
                if let Some(handle) = ctx.editor.mode.picker_state.plugin_handle {
                    let lsp = ctx.plugins.get_as::<LspPlugin>(handle);
                    if let Some(client) =
                        lsp.picker_client_handle.take().and_then(|h| lsp.get_mut(h))
                    {
                        client.cancel_current_request();
                    }
                }

                ctx.editor.enter_mode(ModeKind::default());
                Some(EditorControlFlow::Continue)
            }
        }
    }

    editor.read_line.set_prompt("workspace symbol:");
    editor.picker.filter(WordIndicesIter::empty(), "");
    editor.picker.move_cursor(0);

    if editor.picker.len() > 0 {
        let state = &mut editor.mode.picker_state;
        state.on_client_keys = on_client_keys;
        state.plugin_handle = Some(plugin_handle);
        editor.enter_mode(ModeKind::Picker);
        ClientOperation::EnteredPickerMode
    } else {
        client.cancel_current_request();
        ClientOperation::None
    }
}
