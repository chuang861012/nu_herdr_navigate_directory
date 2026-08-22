use nu_plugin::{MsgPackSerializer, serve_plugin};
use nu_plugin_herdr_cd::HerdrCdPlugin;

fn main() {
    serve_plugin(&HerdrCdPlugin, MsgPackSerializer);
}
