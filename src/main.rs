mod alloc;
mod bundle_css;
mod clean;
mod cli;
mod downloader;
mod extract_css;
mod rewriter;
mod scanner;
mod towebp;
mod translate;
mod webp_encode;
mod zap;

#[cfg(feature = "count-alloc")]
#[global_allocator]
static GLOBAL: alloc::Counter = alloc::Counter;

fn main() {
    let code = cli::run();
    #[cfg(feature = "count-alloc")]
    alloc::print_stats();
    if code != 0 {
        std::process::exit(code);
    }
}
