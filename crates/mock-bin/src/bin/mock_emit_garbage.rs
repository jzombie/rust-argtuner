//! Mock subprocess emitting only unparseable protocol-looking output.
//!
//! Prints a log line that merely mentions the prefix plus a truncated JSON
//! line, then exits 0. Used to prove garbage output fails loudly at metric
//! extraction (missing objective metric) instead of blowing up the parser
//! or — worse — scoring the trial on nothing.

fn main() {
    println!("see ::ARGTUNER:: docs for the event protocol");
    println!(
        "::ARGTUNER::{{\"type\":\"event\",\"name\":\"model.epoch_end\",\"fields\":{{\"metric\":"
    );
    println!("training done");
}
