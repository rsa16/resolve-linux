mod cli;
mod detect;
mod exec;

fn main() {
    std::process::exit(cli::run(std::env::args_os()));
}
