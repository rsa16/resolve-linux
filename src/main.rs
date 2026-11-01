mod cli;
mod detect;
mod exec;
mod ui;

fn main() {
    std::process::exit(cli::run(std::env::args_os()));
}
