mod application;
mod buffer;
mod buffer_position;
mod buffer_view;
mod client;
mod client_event;
mod command;
mod config;
mod connection;
mod cursor;
mod editor;
mod editor_operation;
mod event_manager;
mod history;
mod keymap;
mod mode;
mod pattern;
mod script;
mod script_bindings;
mod select;
mod serialization;
mod syntax;
mod theme;
mod tui;

fn main() {
    let mut client_target_map = editor::ClientTargetMap::default();
    let mut operations = editor_operation::EditorOperationSerializer::default();
    let config = config::Config::default();
    let mut keymaps = keymap::KeyMapCollection::default();
    let mut buffers = buffer::BufferCollection::default();
    let mut buffer_views = buffer_view::BufferViewCollection::default();
    let mut current_buffer_view_handle = None;

    let mut scripts = script::ScriptEngine::new();

    let context = script::ScriptContext {
        target_client: connection::TargetClient::All,
        client_target_map: &mut client_target_map,
        operations: &mut operations,

        config: &config,
        keymaps: &mut keymaps,
        buffers: &mut buffers,
        buffer_views: &mut buffer_views,
        current_buffer_view_handle: &mut current_buffer_view_handle,
    };

    script_bindings::bind_all(&mut scripts).unwrap();
    let r = scripts.eval(context, "print(\"asd\")");
    dbg!(r);
    return;

    if let Err(e) = application::run() {
        eprintln!("{}", e);
    }
}
