//! Bounded exhaustive tests for the build-standard readers.
//!
//! The readers are handwritten text parsers, so a fixed list of cases cannot say
//! they hold over the forms a file can take. Each test here walks every
//! combination of a small alphabet of tokens, quoting, comments and whitespace up
//! to a fixed length, and checks an invariant derived from the input alone rather
//! than from another reader. It needs no dependency: the generators are plain
//! iterators, and the bounds keep the whole suite to a fraction of a second.

use super::{
    config::{Flags, LINKER_FLAG, Pin, THREADS_FLAG, config_problems},
    make::{Assignment, assigned_rustflags},
};

/// Returns every sequence of up to `max_len` items drawn from `alphabet`.
pub fn sequences<T: Clone>(alphabet: &[T], max_len: usize) -> Vec<Vec<T>> {
    let mut all: Vec<Vec<T>> = vec![Vec::new()];
    let mut frontier: Vec<Vec<T>> = vec![Vec::new()];
    for _ in 0..max_len {
        frontier = frontier
            .iter()
            .flat_map(|prefix| {
                alphabet.iter().map(move |item| {
                    let mut next = prefix.clone();
                    next.push(item.clone());
                    next
                })
            })
            .collect();
        all.extend(frontier.iter().cloned());
    }
    all
}

/// What a generated toolchain line is, declared beside its text so that the expectation never
/// re-parses the text it judges.
#[derive(Clone, Copy)]
enum Line {
    /// A table header, naming the table that follows.
    Table(&'static str),
    /// A well-formed `channel` declaration, with the class the standard gives its value
    /// (`None` for a channel the standard does not know).
    Channel(Option<Pin>),
    /// A `channel` key whose value is malformed.
    Malformed,
    /// A lookalike key, a comment or a blank line.
    Other,
}

/// Scenario: every toolchain file made of up to three lines drawn from channel entries,
/// malformed declarations, lookalikes, comments and table headers.
///
/// Invariant: `Pin::read` accepts a file exactly when one well-formed `channel` declaration sits
/// under `[toolchain]` with a known value and no malformed one does, and then returns that value's
/// class; any other file, with none, a repeated, a malformed, a lookalike or a misplaced key, is
/// refused.
#[test]
fn the_pin_reader_agrees_with_an_independent_count_over_every_small_file() {
    let lines = [
        ("[toolchain]", Line::Table("toolchain")),
        ("[other]", Line::Table("other")),
        (
            "channel = \"nightly-2026-05-28\"",
            Line::Channel(Some(Pin::Nightly)),
        ),
        ("channel=\"stable\"", Line::Channel(Some(Pin::Stable))),
        ("channel = \"1.94.0\"", Line::Channel(Some(Pin::Stable))),
        ("channel = \"nightly-preview\"", Line::Channel(None)),
        ("channel = \"nightly-2026-5-28\"", Line::Channel(None)),
        ("channel = \"nightly\"", Line::Channel(None)),
        (
            "channel = \"stable\" # pinned",
            Line::Channel(Some(Pin::Stable)),
        ),
        ("channel = stable", Line::Malformed),
        ("channel = \"stable", Line::Malformed),
        ("channel = \"stable\" junk", Line::Malformed),
        ("channel_x = \"stable\"", Line::Other),
        ("# channel = \"nightly-2026-05-28\"", Line::Other),
        ("", Line::Other),
    ];
    for file in sequences(&lines, 3) {
        let text = file
            .iter()
            .map(|(text, _)| *text)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            Pin::read(&text).map_err(|_| ()),
            expected_read(&file),
            "file: {text:?}"
        );
    }
}

/// What the generated lines of one toolchain file add up to, tracked from the kinds declared
/// beside each line.
#[derive(Default)]
struct Summary {
    table: &'static str,
    found: Vec<Option<Pin>>,
    malformed: bool,
}

impl Summary {
    /// Notes one line's declared kind.
    fn note(&mut self, kind: Line) {
        let in_toolchain = self.table == "toolchain";
        match kind {
            Line::Table(name) => self.table = name,
            Line::Channel(class) if in_toolchain => self.found.push(class),
            Line::Malformed if in_toolchain => self.malformed = true,
            Line::Channel(_) | Line::Malformed | Line::Other => {}
        }
    }

    /// Returns what `Pin::read` should answer: the class of the one well-formed declaration, or an
    /// error for none, a repeated one, an unknown one or a malformed one beside it.
    fn expected(&self) -> Result<Pin, ()> {
        match self.found.as_slice() {
            [class] if !self.malformed => class.ok_or(()),
            _ => Err(()),
        }
    }
}

/// Returns what `Pin::read` should answer for a generated file.
fn expected_read(file: &[(&str, Line)]) -> Result<Pin, ()> {
    let mut summary = Summary::default();
    for (_, kind) in file {
        summary.note(*kind);
    }
    summary.expected()
}

/// Returns the flag list with each bare `-C` joined to the word after it.
fn joined_pairs(list: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut after_c = false;
    for word in list {
        match out.last_mut() {
            Some(tail) if after_c => tail.push_str(word),
            _ => out.push((*word).to_owned()),
        }
        after_c = !after_c && *word == "-C";
    }
    out
}

/// Scenario: every flag list of up to four words from the frontend flag, the
/// linker flag in both spellings, a bare `-C` and ordinary words.
///
/// Invariant: a `-C` followed by its value reads the same as the joined spelling,
/// and the list names the frontend flag and the linker exactly when a word of the
/// joined spelling does.
#[test]
fn flag_lists_read_the_same_in_both_spellings_over_every_short_list() {
    let words = [
        THREADS_FLAG,
        "-C",
        "link-arg=-fuse-ld=mold",
        LINKER_FLAG,
        "-D",
        "warnings",
    ];
    for list in sequences(&words, 4) {
        let flags = Flags::from_words(list.iter().copied());
        let joined = joined_pairs(&list);
        assert_eq!(
            flags.names_threads(),
            joined.iter().any(|w| w == THREADS_FLAG),
            "list: {list:?}"
        );
        assert_eq!(
            flags.names_linker(),
            joined.iter().any(|w| w == LINKER_FLAG),
            "list: {list:?}"
        );
        assert_eq!(
            flags,
            Flags::from_words(joined.iter().map(String::as_str)),
            "list: {list:?}"
        );
    }
}

/// One section of a generated configuration: its header and an optional
/// `rustflags` line.
type Section = (&'static str, Option<&'static str>);

/// Renders one section, optionally decorated with a comment, a lookalike key,
/// a blank line and indentation, none of which is part of the meaning.
fn render_section(section: &Section, decorate: bool) -> Vec<String> {
    let (header, flags) = *section;
    if !decorate {
        return std::iter::once(header.to_owned())
            .chain(flags.map(str::to_owned))
            .collect();
    }
    let decorated_flags = flags.map(|line| format!("  {line} # kept"));
    [
        format!("{header} # a comment"),
        "rustflags_extra = [\"-Dwarnings\"]".to_owned(),
        String::new(),
    ]
    .into_iter()
    .chain(decorated_flags)
    .collect()
}

/// Renders a whole configuration from its sections.
fn render_config(config: &[Section], decorate: bool) -> String {
    config
        .iter()
        .flat_map(|section| render_section(section, decorate))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Scenario: every Cargo configuration of up to three sections drawn from the
/// build table, the all-Linux table, a triple table and an unrelated table, each
/// with or without a `rustflags` line.
///
/// Invariant: what a file says does not change when comments, indentation, blank
/// lines or a lookalike `rustflags_extra` key are added, because none of them is
/// part of the meaning.
#[test]
fn the_configuration_reader_ignores_comments_spacing_and_lookalike_keys() {
    let sections: [Section; 6] = [
        ("[build]", Some("rustflags = [\"-Zthreads=8\"]")),
        ("[build]", None),
        (
            "[target.'cfg(target_os = \"linux\")']",
            Some("rustflags = [\"-Zthreads=8\", \"-Clink-arg=-fuse-ld=mold\"]"),
        ),
        (
            "[target.'cfg(target_os = \"linux\")']",
            Some("rustflags = [\"-Zthreads=8\"]"),
        ),
        (
            "[target.x86_64-unknown-linux-gnu]",
            Some("rustflags = [\"-Zthreads=8\", \"-Clink-arg=-fuse-ld=mold\"]"),
        ),
        ("[env]", None),
    ];
    for config in sequences(&sections, 3) {
        for pin in [Pin::Nightly, Pin::Stable] {
            let plain = config_problems(&render_config(&config, false), pin);
            let decorated = config_problems(&render_config(&config, true), pin);
            assert_eq!(
                plain,
                decorated,
                "config: {:?}",
                render_config(&config, false)
            );
        }
    }
}

/// One generated `RUSTFLAGS` assignment: what it inherits, how that is separated
/// from the flag word, the word, and whether the command is continued.
struct AssignmentCase {
    expansion: &'static str,
    separator: &'static str,
    word: &'static str,
    continued: bool,
}

impl AssignmentCase {
    /// Returns the `make -n` line this case stands for.
    fn line(&self) -> String {
        let command = if self.continued {
            " \\\n  cargo test"
        } else {
            " cargo test"
        };
        format!(
            "RUSTFLAGS=\"{}{}{}\"{command}",
            self.expansion, self.separator, self.word
        )
    }

    /// Returns whether the shell would glue the next flag onto the caller's flags.
    fn is_glued(&self) -> bool {
        self.expansion == "${RUSTFLAGS-}" && self.separator.is_empty() && !self.word.is_empty()
    }

    /// Checks the reader's verdict on this case against what the shell would make of it.
    fn check(&self) {
        let line = self.line();
        let result = assigned_rustflags(&line.replace("\\\n", " "));
        if self.is_glued() {
            assert!(result.is_err(), "glued form accepted: {line:?}");
            return;
        }
        let Ok(Assignment::Flags(flags, inherits)) = result else {
            panic!("a well-formed assignment was refused: {line:?}");
        };
        assert_eq!(inherits, !self.expansion.is_empty(), "line: {line:?}");
        assert_eq!(
            flags.names_threads(),
            self.word.contains(THREADS_FLAG),
            "line: {line:?}"
        );
        assert!(!flags.names_linker(), "line: {line:?}");
    }
}

/// Scenario: every `RUSTFLAGS` assignment built from an inheritance expansion, an
/// optional separator, a flag word and an optional continuation.
///
/// Invariant: an assignment is read as inheriting exactly when it holds an
/// expansion; the empty-fallback expansion glued to the next word is refused,
/// because the shell would join the two into one token; and the reader never
/// reads the expansion as a flag.
#[test]
fn the_assignment_reader_handles_every_inheritance_form() {
    let expansions = ["", "${RUSTFLAGS:+$RUSTFLAGS }", "${RUSTFLAGS-}"];
    let separators = ["", " "];
    let words = ["", THREADS_FLAG, "-D warnings"];
    let prefixes: Vec<(&'static str, &'static str)> = expansions
        .iter()
        .copied()
        .flat_map(|expansion| {
            separators
                .iter()
                .copied()
                .map(move |separator| (expansion, separator))
        })
        .collect();
    let spelled: Vec<(&'static str, &'static str, &'static str)> = prefixes
        .iter()
        .copied()
        .flat_map(|(expansion, separator)| {
            words
                .iter()
                .copied()
                .map(move |word| (expansion, separator, word))
        })
        .collect();
    for (expansion, separator, word) in spelled {
        for continued in [false, true] {
            AssignmentCase {
                expansion,
                separator,
                word,
                continued,
            }
            .check();
        }
    }
}
