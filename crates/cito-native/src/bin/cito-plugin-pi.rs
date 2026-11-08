#[path = "../../../../plugins/pi/native.rs"]
mod plugin;
#[path = "../plugin_support.rs"]
mod support;
fn main() {
    support::one_shot(plugin::handle);
}
