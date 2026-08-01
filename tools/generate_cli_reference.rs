use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let output_path = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("docs/reference/cli-reference.md"));

    let output = Command::new("cargo")
        .args(["run", "--quiet", "-p", "nca-cli", "--", "--help"])
        .output()
        .expect("failed to run nca --help");

    if !output.status.success() {
        eprintln!("cargo run -p nca-cli -- --help failed");
        std::process::exit(output.status.code().unwrap_or(1));
    }

    let help = String::from_utf8(output.stdout).expect("nca help was not UTF-8");
    let document = format!(
        "# CLI Reference\n\n**Status:** Generated snapshot  \n**Source:** `nca --help` from the Clap command definitions\n\nThis page is generated from the current CLI. For task-oriented workflows and examples, see the [user command guide](../user/commands.md).\n\n```text\n{help}```\n"
    );

    fs::write(&output_path, document)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", output_path.display()));
}
