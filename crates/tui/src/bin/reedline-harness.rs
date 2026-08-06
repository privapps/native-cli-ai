use nca_tui::Repl;
use reedline::{DefaultPrompt, Signal};
use std::path::PathBuf;

fn main() {
    let history = std::env::var_os("NCA_REEDLINE_HISTORY").map(PathBuf::from);
    let mut editor = Repl::build_line_editor(history).expect("build production line editor");
    match editor.read_line(&DefaultPrompt::default()) {
        Ok(Signal::Success(input)) => {
            println!(
                "NCA_REEDLINE_SUCCESS={}",
                serde_json::to_string(&input).unwrap()
            );
        }
        Ok(Signal::CtrlD) => println!("NCA_REEDLINE_CTRLD"),
        Ok(Signal::CtrlC) => println!("NCA_REEDLINE_CTRLC"),
        Ok(_) => println!("NCA_REEDLINE_OTHER"),
        Err(error) => {
            eprintln!("NCA_REEDLINE_ERROR={error}");
            std::process::exit(1);
        }
    }
}
