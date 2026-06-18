mod clean;
mod cli;
mod downloader;
mod rewriter;
mod scanner;
mod zap;

fn main() {
    let code = cli::run();
    if code != 0 {
        std::process::exit(code);
    }
}
