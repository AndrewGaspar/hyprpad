//! The committed test fixture: `osk/data/fixture.words` → `osk/data/fixture.model`.
//!
//! The real model is built from multi-gigabyte downloads
//! (`osk/tools/build-model/`) and is far too large to commit, so the fixture is
//! what the test suite predicts against: a ~130-word hand-authored list in the
//! pipeline's own text format, and the model built from it by the same code the
//! pipeline uses.
//!
//! These tests are what keep the two honest — that the build is reproducible
//! byte for byte, and that the committed artifact still opens through the real
//! mmap path (the unit tests build in memory, so this is the only place
//! [`Model::open`] itself is exercised).

use std::path::PathBuf;

use hyprpad_osk::predict::{model, wordlist, Context, Predictor};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data").join(name)
}

#[test]
fn the_committed_fixture_model_is_exactly_what_the_builder_produces() {
    let words = std::fs::read_to_string(data("fixture.words")).expect("fixture.words");
    let attribution = std::fs::read_to_string(data("fixture.attribution")).expect("attribution");
    let entries = wordlist::parse(&words).expect("fixture word list parses");
    let built = model::build(&entries, attribution.trim()).expect("fixture model builds");
    let committed = std::fs::read(data("fixture.model")).expect("fixture.model");
    assert_eq!(
        built,
        committed,
        "data/fixture.model is stale — rebuild it with\n  \
         cargo run --bin hyprpad-osk-build-model -- \
         data/fixture.words data/fixture.model --attribution-file data/fixture.attribution"
    );
}

#[test]
fn the_committed_fixture_model_opens_through_the_mmap_path() {
    let m = model::Model::open(&data("fixture.model")).expect("fixture.model opens");
    assert_eq!(m.len(), 137);
    assert!(m.attribution().contains("fixture"));
    let the = m.id("the").expect("the");
    assert_eq!(m.word(the), Some("the"));
    assert!(!m.successors(the).is_empty());
    // Casing survives: the fixture holds both "wayland" and "Wayland".
    assert!(m.id("wayland").is_some() && m.id("Wayland").is_some());
}

#[test]
fn a_predictor_over_the_committed_model_completes_and_predicts() {
    let m = model::Model::open(&data("fixture.model")).unwrap();
    let p = Predictor::with_model(m);
    assert!(p.is_ready());

    let mut ctx = Context::new();
    ctx.push_str("hello ");
    let got = p.candidates(&ctx, "", 3);
    let next: Vec<&str> = got.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(next, vec!["world", "there", "hyprland"]);

    let mut ctx = Context::new();
    ctx.push_str("the k");
    assert_eq!(p.candidates(&ctx, "k", 1)[0].text, "keyboard");
}

/// The one test that touches a *real* model, when there is one to touch.
///
/// The shipped model is built, not committed (see
/// `osk/tools/build-model/DATA-LICENSES.md`), so this is a no-op unless
/// `$HYPRPAD_OSK_MODEL` points at one — which is exactly how the keyboard finds
/// it too. Run it after a build:
///
/// ```sh
/// HYPRPAD_OSK_MODEL=~/.local/share/hyprpad-osk/en.model cargo test --test fixture_model -- --nocapture
/// ```
#[test]
fn a_real_model_opens_predicts_and_stays_inside_the_latency_budget() {
    let Some(path) = std::env::var_os("HYPRPAD_OSK_MODEL").map(PathBuf::from) else {
        eprintln!("HYPRPAD_OSK_MODEL unset — skipping the real-model check");
        return;
    };
    let m = model::Model::open(&path).expect("the named model opens");
    eprintln!("model: {} words, {}", m.len(), m.attribution().lines().next().unwrap_or(""));
    assert!(m.len() > 10_000, "a real English model has tens of thousands of words");

    let p = Predictor::with_model(m);
    let mut ctx = Context::new();
    ctx.push_str("this is an ex");
    let got = p.candidates(&ctx, "ex", 3);
    assert!(!got.is_empty());
    eprintln!("  \"this is an ex\" -> {:?}", got.iter().map(|c| &c.text).collect::<Vec<_>>());

    // A single-letter prefix after a common word is the worst case: the widest
    // fst range and a full successor list.
    let mut worst = Context::new();
    worst.push_str("the s");
    for _ in 0..50 {
        let _ = p.candidates(&worst, "s", 3);
    }
    let start = std::time::Instant::now();
    const N: u32 = 500;
    for _ in 0..N {
        let _ = p.candidates(&worst, "s", 3);
    }
    let per = start.elapsed() / N;
    eprintln!("  worst-case query: {per:?} (budget 1 ms, bound 5 ms)");
    assert!(per.as_micros() < 5_000, "real-model query took {per:?}");
}

#[test]
fn the_builder_binary_rebuilds_the_fixture_byte_for_byte() {
    // The binary is what the pipeline's last step actually runs, so exercise it
    // rather than only the library call it wraps.
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_hyprpad-osk-build-model"));
    let out = std::env::temp_dir().join(format!("hyprpad-osk-fixture-{}.model", std::process::id()));
    let status = std::process::Command::new(&exe)
        .arg(data("fixture.words"))
        .arg(&out)
        .arg("--attribution-file")
        .arg(data("fixture.attribution"))
        .status()
        .expect("run hyprpad-osk-build-model");
    assert!(status.success(), "builder exited with {status}");
    let built = std::fs::read(&out).expect("built model");
    let committed = std::fs::read(data("fixture.model")).expect("fixture.model");
    assert_eq!(built, committed);
    let _ = std::fs::remove_file(&out);
}
