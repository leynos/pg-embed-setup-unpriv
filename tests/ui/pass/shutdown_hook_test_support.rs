use pg_embedded_setup_unpriv::test_support::{PostmasterPid, process_is_running, read_postmaster_pid};

fn main() {
    let _reader = read_postmaster_pid;
    let _runner: fn(PostmasterPid) -> bool = process_is_running;
}
