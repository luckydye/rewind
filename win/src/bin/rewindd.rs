//! Console build of the daemon: the CLI, and the daemon itself when started
//! from a terminal (its log goes there).

fn main() {
    rewindd::initialize();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(message) = rewindd::cli::main(&args) {
        eprintln!("rewindd: {message}");
        std::process::exit(1);
    }
}
