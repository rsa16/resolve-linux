mod catalog;
mod cli;
mod detect;
mod exec;
mod privilege;
mod ui;

fn main() {
    std::process::exit(cli::run(std::env::args_os()));
}
