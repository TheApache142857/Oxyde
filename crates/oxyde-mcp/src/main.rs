//! `oxyde-mcp`: a thin wrapper around [`oxyde_mcp::run`], which does the work so that
//! tests can drive the same server without a process.

fn main() {
    let code = oxyde_mcp::run(
        std::env::args_os(),
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    std::process::exit(code);
}
