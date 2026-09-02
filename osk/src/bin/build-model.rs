//! `hyprpad-osk-build-model` — turn a word list into a prediction model.
//!
//! The second half of the offline pipeline described under "Prediction" in
//! `osk/README.md`: the Python script downloads and merges the
//! data sources and writes the plain-text word list
//! ([`hyprpad_osk::predict::wordlist`]); this writes the binary model the
//! keyboard maps at runtime ([`hyprpad_osk::predict::model`]).
//!
//! ```text
//! hyprpad-osk-build-model <words.txt> <out.model> [--attribution <text>|--attribution-file <path>]
//! ```
//!
//! The build is deterministic: the same word list and attribution always produce
//! the same bytes, which is what lets the committed test fixture be checked
//! against a rebuild.

use std::path::PathBuf;
use std::process::ExitCode;

use hyprpad_osk::predict::{model, wordlist};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut positional: Vec<String> = Vec::new();
    let mut attribution = String::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            "--attribution" => match args.next() {
                Some(t) => attribution = t,
                None => return fail("--attribution needs a value"),
            },
            "--attribution-file" => match args.next() {
                Some(p) => match std::fs::read_to_string(&p) {
                    Ok(t) => attribution = t.trim().to_string(),
                    Err(e) => return fail(&format!("read {p}: {e}")),
                },
                None => return fail("--attribution-file needs a path"),
            },
            other if other.starts_with('-') => {
                return fail(&format!("unknown option '{other}' (try --help)"))
            }
            other => positional.push(other.to_string()),
        }
    }
    let [words, out] = match positional.as_slice() {
        [a, b] => [PathBuf::from(a), PathBuf::from(b)],
        _ => return fail("usage: hyprpad-osk-build-model <words.txt> <out.model> (try --help)"),
    };

    let text = match std::fs::read_to_string(&words) {
        Ok(t) => t,
        Err(e) => return fail(&format!("read {}: {e}", words.display())),
    };
    let entries = match wordlist::parse(&text) {
        Ok(e) => e,
        Err(e) => return fail(&format!("{}: {e}", words.display())),
    };
    let successors: usize = entries.iter().map(|e| e.successors.len()).sum();
    let bytes = match model::build(&entries, &attribution) {
        Ok(b) => b,
        Err(e) => return fail(&e),
    };
    if let Err(e) = std::fs::write(&out, &bytes) {
        return fail(&format!("write {}: {e}", out.display()));
    }
    // Read it straight back, so a model that cannot be opened is never shipped.
    match model::Model::open(&out) {
        Ok(m) => eprintln!(
            "hyprpad-osk-build-model: {} — {} words, {} bigrams, {:.2} MB",
            out.display(),
            m.len(),
            successors,
            bytes.len() as f64 / (1024.0 * 1024.0),
        ),
        Err(e) => return fail(&format!("the model just written does not read back: {e}")),
    }
    ExitCode::SUCCESS
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("hyprpad-osk-build-model: {msg}");
    ExitCode::FAILURE
}

fn print_help() {
    print!(
        "hyprpad-osk-build-model — build the on-screen keyboard's prediction model\n\
         \n\
         USAGE:\n\
           hyprpad-osk-build-model <words.txt> <out.model> [OPTIONS]\n\
         \n\
         OPTIONS:\n\
           --attribution <text>       data provenance line stored in the model\n\
           --attribution-file <path>  read that line from a file\n\
           -h, --help                 print this help\n\
         \n\
         INPUT FORMAT (osk/data/fixture.words is a worked example):\n\
           the 0.055        a word and its unigram probability, one per line\n\
             quick 0.02     indented: a successor and S(successor | the)\n\
           # comments and blank lines are ignored\n\
         \n\
         The pipeline that produces a real word list from wordfreq, SCOWL and a\n\
         bigram corpus lives in osk/tools/build-model/.\n"
    );
}
