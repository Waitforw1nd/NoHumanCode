//! Fault-injection worker for the separately authorized W02 test.
#[cfg(windows)]
fn main() {
    use peachsh::wasm_worker_protocol::{self as protocol, Decision, Outcome, Response};
    use std::io::{self, Write};
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};

    let request = protocol::read_request(&mut io::stdin().lock()).expect("complete request");
    let response = Response {
        manifest: request.manifest,
        outcome: Outcome::Ok {
            decision: Decision::Stop {},
        },
    };
    let stdout = io::stdout();
    let mut output = stdout.lock();
    protocol::write_response(&mut output, &response).expect("response");
    output.flush().expect("flush response");
    let mut written = 0;
    let ok = unsafe {
        WriteFile(
            GetStdHandle(STD_OUTPUT_HANDLE),
            std::ptr::null(),
            0,
            &mut written,
            std::ptr::null_mut(),
        )
    };
    assert!(ok != 0 && written == 0, "zero-byte WriteFile failed");
    output.write_all(b"x").expect("trailing byte");
    output.flush().expect("flush trailing byte");
}

#[cfg(not(windows))]
fn main() {
    panic!("W02 fault probe requires Windows");
}
