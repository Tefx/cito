#[path = "../../../../plugins/pi/native.rs"]
mod plugin;
use cito_native::support;
fn main() {
    support::one_shot(plugin::handle);
}
