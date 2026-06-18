mod cli;
mod downloader;
mod rewriter;
mod scanner;

fn main() {
    let code = cli::run();
    if code != 0 {
        std::process::exit(code);
    }
}
