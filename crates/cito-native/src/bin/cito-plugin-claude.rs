#[path = "../../../../plugins/claude/native.rs"]
pub mod plugin;
use cito_native::support;

fn main() {
    plugin::main();
}
