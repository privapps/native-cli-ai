use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use reedline::{History, SearchDirection, SearchQuery};
use std::io::{Read, Write};
use std::sync::Arc;

#[test]
fn bracketed_multiline_paste_reaches_production_reedline_as_one_entry() {
    let pty_system = NativePtySystem::default();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open terminal harness");
    let history_dir = tempfile::tempdir().expect("history directory");
    let history_path = history_dir.path().join("history");

    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_reedline-harness"));
    command.env("TERM", "xterm-256color");
    command.env("NCA_REEDLINE_HISTORY", &history_path);
    let mut child = pair
        .slave
        .spawn_command(command)
        .expect("spawn Reedline harness");
    drop(pair.slave);

    let mut writer = pair.master.take_writer().expect("terminal writer");
    let mut reader = pair.master.try_clone_reader().expect("terminal reader");
    let draft = "first paragraph\n\nsecond paragraph";
    writer
        // Reedline asks the terminal for the cursor position when entering
        // raw mode. A PTY is not a terminal emulator, so provide the normal
        // origin response before sending the bracketed paste payload.
        .write_all(format!("\x1b[1;1R\x1b[200~{draft}\x1b[201~\r").as_bytes())
        .expect("write bracketed paste");
    writer.flush().expect("flush bracketed paste");
    drop(writer);

    let output = Arc::new(std::sync::Mutex::new(Vec::new()));
    let output_reader = Arc::clone(&output);
    let reader_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .expect("read terminal output");
        *output_reader.lock().expect("output lock") = bytes;
    });
    let status = child.wait().expect("wait for Reedline harness");
    reader_thread.join().expect("join terminal reader");

    let output = output.lock().expect("output lock").clone();
    assert!(status.success(), "harness output: {:?}", output);
    let output = String::from_utf8_lossy(&output);
    let marker = "NCA_REEDLINE_SUCCESS=";
    assert_eq!(
        output.matches(marker).count(),
        1,
        "expected exactly one submitted Reedline buffer: {output:?}"
    );
    let line = output
        .split(['\r', '\n'])
        .find_map(|line| line.find(marker).map(|idx| &line[idx + marker.len()..]))
        .expect("success marker in terminal output");
    let submitted: String = serde_json::from_str(line).expect("decode submitted draft");
    assert_eq!(submitted, draft);
    assert!(
        output.contains("\x1b[?2004l"),
        "production Reedline did not disable bracketed paste on exit: {output:?}"
    );

    let history = reedline::FileBackedHistory::with_file(100, history_path)
        .expect("reload production history");
    let entries = history
        .search(SearchQuery::everything(SearchDirection::Forward, None))
        .expect("search production history");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command_line, draft);
}
