#[path = "../../../../plugins/telegram/native.rs"]
mod plugin;
#[path = "../plugin_support.rs"]
mod support;
fn main() {
    plugin::main();
}
