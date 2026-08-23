use nu_plugin::{MsgPackSerializer, serve_plugin};
use nu_plugin_herdr_navigate_directory::HerdrNavigateDirectoryPlugin;

fn main() {
    serve_plugin(&HerdrNavigateDirectoryPlugin, MsgPackSerializer);
}
