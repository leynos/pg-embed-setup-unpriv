use pg_embedded_setup_unpriv::BootstrapResult;
use pg_embedded_setup_unpriv::test_support::{
    PostmasterPid, process_is_running, read_postmaster_pid,
};
use std::path::Path;

pub fn verify_surface() {
    let _reader: fn(&Path) -> BootstrapResult<Option<PostmasterPid>> = read_postmaster_pid;
    let _runner: fn(PostmasterPid) -> bool = process_is_running;
}

fn main() {
    verify_surface();
}
