use std::fs;

use crate::{
    command::{CommandManager, CommandTokenizer, CompletionSource},
    editor::KeysIterator,
    editor_utils::{hash_bytes, ReadLinePoll},
    mode::{Mode, ModeContext, ModeKind, ModeOperation, ModeState},
    picker::Picker,
    platform::Key,
    word_database::WordIndicesIter,
};

enum ReadCommandState {
    NavigatingHistory(usize),
    TypingCommand,
}

pub struct State {
    read_state: ReadCommandState,
    completion_index: usize,
    completion_source: CompletionSource,
    completion_path_hash: Option<u64>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            read_state: ReadCommandState::TypingCommand,
            completion_index: 0,
            completion_source: CompletionSource::Custom(&[]),
            completion_path_hash: None,
        }
    }
}

impl ModeState for State {
    fn on_enter(ctx: &mut ModeContext) {
        let state = &mut ctx.editor.mode.command_state;
        state.read_state = ReadCommandState::NavigatingHistory(ctx.editor.commands.history_len());
        state.completion_index = 0;
        state.completion_source = CompletionSource::Custom(&[]);
        state.completion_path_hash = None;

        ctx.editor.read_line.set_prompt(":");
        ctx.editor.read_line.input_mut().clear();
        ctx.editor.picker.clear();
    }

    fn on_exit(ctx: &mut ModeContext) {
        ctx.editor.read_line.input_mut().clear();
        ctx.editor.picker.clear();
    }

    fn on_client_keys(ctx: &mut ModeContext, keys: &mut KeysIterator) -> Option<ModeOperation> {
        let state = &mut ctx.editor.mode.command_state;
        match ctx.editor.read_line.poll(
            ctx.platform,
            &mut ctx.editor.string_pool,
            &ctx.editor.buffered_keys,
            keys,
        ) {
            ReadLinePoll::Pending => {
                keys.put_back();
                match keys.next(&ctx.editor.buffered_keys) {
                    Key::Ctrl('n' | 'j') => match state.read_state {
                        ReadCommandState::NavigatingHistory(ref mut i) => {
                            *i = ctx
                                .editor
                                .commands
                                .history_len()
                                .saturating_sub(1)
                                .min(*i + 1);
                            let entry = ctx.editor.commands.history_entry(*i);
                            let input = ctx.editor.read_line.input_mut();
                            input.clear();
                            input.push_str(entry);
                        }
                        ReadCommandState::TypingCommand => apply_completion(ctx, 1),
                    },
                    Key::Ctrl('p' | 'k') => match state.read_state {
                        ReadCommandState::NavigatingHistory(ref mut i) => {
                            *i = i.saturating_sub(1);
                            let entry = ctx.editor.commands.history_entry(*i);
                            let input = ctx.editor.read_line.input_mut();
                            input.clear();
                            input.push_str(entry);
                        }
                        ReadCommandState::TypingCommand => apply_completion(ctx, -1),
                    },
                    _ => update_autocomplete_entries(ctx),
                }
            }
            ReadLinePoll::Canceled => Mode::change_to(ctx, ModeKind::default()),
            ReadLinePoll::Submitted => {
                let input = ctx.editor.read_line.input();
                ctx.editor.commands.add_to_history(input);

                let mut command = ctx.editor.string_pool.acquire_with(input);
                let operation = CommandManager::eval(
                    ctx.editor,
                    ctx.platform,
                    ctx.clients,
                    Some(ctx.client_handle),
                    &mut command,
                )
                .map(From::from);
                ctx.editor.string_pool.release(command);

                if ctx.editor.mode.kind() == ModeKind::Command {
                    Mode::change_to(ctx, ModeKind::default());
                }

                return operation;
            }
        }

        None
    }
}

fn apply_completion(ctx: &mut ModeContext, cursor_movement: isize) {
    ctx.editor.picker.move_cursor(cursor_movement);
    if let Some((_, entry)) = ctx.editor.picker.current_entry(&ctx.editor.word_database) {
        let input = ctx.editor.read_line.input_mut();
        input.truncate(ctx.editor.mode.command_state.completion_index);
        input.push_str(entry);
    }
}

fn update_autocomplete_entries(ctx: &mut ModeContext) {
    let state = &mut ctx.editor.mode.command_state;

    let input = ctx.editor.read_line.input();
    let mut tokens = CommandTokenizer(input);

    let mut last_token = match tokens.next() {
        Some(token) => token,
        None => {
            ctx.editor.picker.clear();
            state.completion_index = input.len();
            state.completion_source = CompletionSource::Custom(&[]);
            if input.trim().is_empty() {
                state.read_state =
                    ReadCommandState::NavigatingHistory(ctx.editor.commands.history_len());
            }
            return;
        }
    };
    let command_name = last_token.trim_end_matches('!');

    if let ReadCommandState::NavigatingHistory(_) = state.read_state {
        state.read_state = ReadCommandState::TypingCommand;
    }

    let mut arg_count = 0;
    for token in tokens {
        arg_count += 1;
        last_token = token;
    }

    let mut pattern = last_token;

    if input.ends_with(&[' ', '\t'][..]) {
        arg_count += 1;
        pattern = &input[input.len()..];
    }

    let mut completion_source = CompletionSource::Custom(&[]);
    if arg_count > 0 {
        match ctx.editor.commands.find_command(command_name) {
            Some(command) => {
                let completion_index = arg_count - 1;
                if completion_index < command.completions.len() {
                    completion_source = command.completions[completion_index];
                }
            }
            _ => (),
        }
    } else {
        completion_source = CompletionSource::Commands;
    }

    state.completion_index = pattern.as_ptr() as usize - input.as_ptr() as usize;

    if state.completion_source != completion_source {
        state.completion_path_hash = None;
        ctx.editor.picker.clear();

        match completion_source {
            CompletionSource::Commands => {
                for command in ctx.editor.commands.builtin_commands() {
                    ctx.editor.picker.add_custom_entry(command.name);
                }
            }
            CompletionSource::Buffers => {
                for buffer in ctx.editor.buffers.iter() {
                    if let Some(path) = buffer.path.to_str() {
                        ctx.editor.picker.add_custom_entry(path);
                    }
                }
            }
            CompletionSource::Custom(completions) => {
                for completion in completions {
                    ctx.editor.picker.add_custom_entry(completion);
                }
            }
            _ => (),
        }
    }

    match completion_source {
        CompletionSource::Files => {
            fn set_files_in_path_as_entries(picker: &mut Picker, path: &str) {
                picker.clear();
                let path = if path.is_empty() { "." } else { path };
                let read_dir = match fs::read_dir(path) {
                    Ok(iter) => iter,
                    Err(_) => return,
                };
                for entry in read_dir {
                    let entry = match entry {
                        Ok(entry) => entry.file_name(),
                        Err(_) => return,
                    };
                    if let Some(entry) = entry.to_str() {
                        picker.add_custom_entry(entry);
                    }
                }
            }

            let (parent, file) = match pattern.rfind('/') {
                Some(i) => pattern.split_at(i + 1),
                None => ("", pattern),
            };

            let parent_hash = hash_bytes(parent.as_bytes());
            if state.completion_path_hash != Some(parent_hash) {
                set_files_in_path_as_entries(&mut ctx.editor.picker, parent);
                state.completion_path_hash = Some(parent_hash);
            }

            state.completion_index = file.as_ptr() as usize - input.as_ptr() as usize;
            pattern = file;
        }
        _ => (),
    }

    state.completion_source = completion_source;
    ctx.editor.picker.filter(WordIndicesIter::empty(), pattern);
}
