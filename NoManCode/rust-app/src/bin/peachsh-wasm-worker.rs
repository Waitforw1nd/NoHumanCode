use peachsh::wasm_worker_protocol::{self as protocol, Outcome, Response};
use std::io::{self, Write};

fn main() {
    let request = match protocol::read_request(&mut io::stdin().lock()) {
        Ok(request) => request,
        Err(_) => {
            let _ = writeln!(io::stderr().lock(), "worker protocol error");
            std::process::exit(1);
        }
    };
    let outcome = match peachsh::wasm_worker_executor::execute(
        &request.manifest,
        &request.input,
        &request.module,
    ) {
        Ok(decision) => Outcome::Ok { decision },
        Err(error) => Outcome::Error { error },
    };
    let response = Response {
        manifest: request.manifest,
        outcome,
    };
    if protocol::write_response(&mut io::stdout().lock(), &response).is_err() {
        let _ = writeln!(io::stderr().lock(), "worker output error");
        std::process::exit(1);
    }
}
