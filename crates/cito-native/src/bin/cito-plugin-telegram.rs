#[path = "../../../../plugins/telegram/native.rs"]
mod plugin;
use cito_native::support;
fn main() {
    plugin::main();
}
