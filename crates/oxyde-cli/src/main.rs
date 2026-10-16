//! `oxyde-cli`: a thin wrapper around [`oxyde_cli::run`], which does the work so that
//! tests can drive the same commands without a process.

fn main() {
    let code = oxyde_cli::run(
        std::env::args_os(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    std::process::exit(code);
}
