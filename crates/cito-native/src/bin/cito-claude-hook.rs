#[path = "../../../../plugins/claude/native.rs"]
mod plugin;
#[path = "../plugin_support.rs"]
mod support;
fn main() {
    plugin::hook_main();
}
