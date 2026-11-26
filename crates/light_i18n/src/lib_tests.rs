use std::collections::HashMap;
use std::path::Path;

use super::{Catalog, Language, SPANISH_FILES, fill, translate};

#[test]
fn placeholders_are_filled_in_order() {
    assert_eq!(fill("{} of {}", &[&1, &3]), "1 of 3");
}

#[test]
fn translations_can_reorder_placeholders() {
    assert_eq!(fill("{1} de {0}", &[&"a", &"b"]), "b de a");
}

#[test]
fn doubled_braces_are_literal() {
    assert_eq!(fill("{{x}} {}", &[&5]), "{x} 5");
}

#[test]
fn stray_braces_are_kept() {
    assert_eq!(fill("a { b } c", &[]), "a { b } c");
}

#[test]
fn missing_translations_fall_back_to_english() {
    assert_eq!(
        translate(Language::Spanish, "No translation for this text"),
        "No translation for this text"
    );
}

#[test]
fn spanish_translations_are_used() {
    assert_eq!(translate(Language::Spanish, "Settings"), "Configuración");
    assert_eq!(translate(Language::English, "Settings"), "Settings");
}

#[test]
fn every_catalog_file_is_valid() {
    for (name, json) in SPANISH_FILES {
        let file: Result<HashMap<String, String>, _> = serde_json::from_str(json);
        assert!(file.is_ok(), "{name}.json: {:?}", file.err());
    }
}

/// Counts the placeholders a text expects: `{}` and `{N}` count as one each.
fn placeholder_count(text: &str) -> usize {
    let text = text.replace("{{", "").replace("}}", "");
    let mut count = 0;
    let mut rest = text.as_str();
    while let Some(position) = rest.find('{') {
        rest = &rest[position..];
        match super::placeholder(rest) {
            Some((_, after)) => {
                count += 1;
                rest = after;
            }
            None => rest = &rest[1..],
        }
    }
    count
}

#[test]
fn translations_keep_their_placeholders() {
    let catalog = Catalog::parse(SPANISH_FILES);
    let wrong: Vec<_> = catalog
        .0
        .iter()
        .filter(|(english, spanish)| placeholder_count(english) != placeholder_count(spanish))
        .collect();
    assert!(wrong.is_empty(), "placeholders differ: {wrong:#?}");
}

#[test]
fn no_text_is_translated_two_ways() {
    let mut seen: HashMap<String, (String, &str)> = HashMap::new();
    for (file, json) in SPANISH_FILES {
        let file_entries: HashMap<String, String> = serde_json::from_str(json).unwrap();
        // serde keeps only the last of two equal keys in one file, so count the keys as written.
        let written = json.matches("\":").count();
        assert_eq!(written, file_entries.len(), "{file}.json repeats a text");
        // Areas may share a text, but the catalog keeps only one translation of it.
        for (english, spanish) in file_entries {
            if let Some((other_spanish, other_file)) = seen.get(&english) {
                assert_eq!(
                    other_spanish, &spanish,
                    "{english:?} differs between {other_file}.json and {file}.json"
                );
            }
            seen.insert(english, (spanish, *file));
        }
    }
}

/// Every text passed to `tr!` or `tr` in the app has a Spanish translation.
#[test]
fn every_translated_text_has_spanish() {
    let catalog = Catalog::parse(SPANISH_FILES);
    let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/src");
    let mut missing = Vec::new();
    for file in rust_files(&app) {
        let source = std::fs::read_to_string(&file).unwrap();
        for text in translated_texts(&source) {
            if catalog.get(&text).is_none() {
                missing.push(format!("{}: {text:?}", file.display()));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "missing Spanish:\n{}",
        missing.join("\n")
    );
}

fn rust_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files
}

/// The string literals passed to `tr!(` or `tr(` in `source`.
fn translated_texts(source: &str) -> Vec<String> {
    let mut texts = Vec::new();
    for call in ["tr!(", "tr("] {
        for (position, _) in source.match_indices(call) {
            let before = source[..position].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            // rustfmt may put the literal on the next line.
            let argument = source[position + call.len()..].trim_start();
            if let Some(literal) = argument.strip_prefix('"') {
                texts.extend(string_literal(literal));
            }
        }
    }
    texts
}

/// Reads a string literal whose opening quote was just consumed.
fn string_literal(rest: &str) -> Option<String> {
    let mut text = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(text),
            '\\' => match chars.next()? {
                // A backslash at the end of a line skips the line break and the indentation.
                '\n' | '\r' => chars = chars.as_str().trim_start().chars(),
                'n' => text.push('\n'),
                't' => text.push('\t'),
                other => text.push(other),
            },
            c => text.push(c),
        }
    }
    None
}

#[test]
fn source_scanning_reads_escapes() {
    assert_eq!(
        translated_texts("x(tr!(\"Say \\\"hi\\\"\", a)); attr(\"no\"); tr(\"Long \\\n    text\")"),
        vec!["Say \"hi\"".to_owned(), "Long text".to_owned()]
    );
}
