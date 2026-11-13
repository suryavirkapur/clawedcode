use clap::Parser;
use clawedcode::{app::run, cli::Cli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Rust ignores SIGPIPE by default, turning closed pipes into panics in
    // `println!`. Restore the default so `clawedcode ... | head` exits
    // quietly like a normal CLI.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    run(Cli::parse()).await
}
