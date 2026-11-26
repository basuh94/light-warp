//! Translations for the user interface.
//!
//! English text is its own key: code keeps writing `tr("Settings")`, and other languages map that
//! text to theirs in `locales/<code>/*.json`. Text that has no translation yet falls back to
//! English, so a partly translated language still works.
//!
//! The language is chosen once at startup with [`set_language`]; changing it needs a restart,
//! because views build much of their text when they are created.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Language {
    #[default]
    English,
    Spanish,
}

impl Language {
    pub const ALL: [Language; 2] = [Language::English, Language::Spanish];

    /// The language's name in that language, as shown in the language picker.
    pub fn native_name(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::Spanish => "Español",
        }
    }

    /// The language Windows shows its own interface in, if we have it.
    pub fn of_system() -> Language {
        #[cfg(windows)]
        {
            // SAFETY: takes no arguments and only reads the user's settings.
            let lang_id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
            const PRIMARY_LANGUAGE_MASK: u16 = 0x3ff;
            const LANG_SPANISH: u16 = 0x0a;
            if lang_id & PRIMARY_LANGUAGE_MASK == LANG_SPANISH {
                return Language::Spanish;
            }
        }
        Language::English
    }

    fn index(self) -> u8 {
        match self {
            Language::English => 0,
            Language::Spanish => 1,
        }
    }

    fn catalog(self) -> Option<&'static Catalog> {
        static SPANISH: OnceLock<Catalog> = OnceLock::new();
        match self {
            Language::English => None,
            Language::Spanish => Some(SPANISH.get_or_init(|| Catalog::parse(SPANISH_FILES))),
        }
    }
}

static ACTIVE: AtomicU8 = AtomicU8::new(0);

pub fn set_language(language: Language) {
    ACTIVE.store(language.index(), Ordering::Relaxed);
}

pub fn language() -> Language {
    Language::ALL
        .into_iter()
        .find(|language| language.index() == ACTIVE.load(Ordering::Relaxed))
        .unwrap_or_default()
}

/// The text in the active language.
pub fn tr(english: &'static str) -> &'static str {
    translate(language(), english)
}

/// Like [`tr`], for text that is not known at compile time. Text without a translation is
/// returned as is.
pub fn tr_dynamic(english: &str) -> String {
    match language()
        .catalog()
        .and_then(|catalog| catalog.get(english))
    {
        Some(translated) => translated.to_owned(),
        None => english.to_owned(),
    }
}

fn translate(language: Language, english: &'static str) -> &'static str {
    language
        .catalog()
        .and_then(|catalog| catalog.get(english))
        .unwrap_or(english)
}

/// Translates a text, filling its placeholders when arguments are given.
///
/// Placeholders are `{}` in the English text. A translation can use `{}` in the same order, or
/// `{0}`, `{1}`… to put them in a different order. `{{` and `}}` are literal braces.
///
/// ```
/// use light_i18n::tr;
/// assert_eq!(tr!("Settings"), "Settings");
/// assert_eq!(tr!("{} of {}", 1, 3), "1 of 3");
/// ```
#[macro_export]
macro_rules! tr {
    ($text:literal) => {
        $crate::tr($text)
    };
    ($text:literal, $($arg:expr),+ $(,)?) => {
        $crate::fill($crate::tr($text), &[$(&$arg as &dyn ::std::fmt::Display),+])
    };
}

/// Replaces the placeholders in `template` with `args`. Used by [`tr!`].
pub fn fill(template: &str, args: &[&dyn Display]) -> String {
    let mut output = String::with_capacity(template.len() + 16 * args.len());
    let mut next_arg = 0;
    let mut rest = template;
    while let Some(position) = rest.find(['{', '}']) {
        output.push_str(&rest[..position]);
        rest = &rest[position..];
        if let Some(after) = rest.strip_prefix("{{") {
            output.push('{');
            rest = after;
        } else if let Some(after) = rest.strip_prefix("}}") {
            output.push('}');
            rest = after;
        } else if let Some((index, after)) = placeholder(rest) {
            let index = index.unwrap_or_else(|| {
                next_arg += 1;
                next_arg - 1
            });
            if let Some(arg) = args.get(index) {
                output.push_str(&arg.to_string());
            }
            rest = after;
        } else {
            output.push_str(&rest[..1]);
            rest = &rest[1..];
        }
    }
    output.push_str(rest);
    output
}

/// Parses `{}` or `{N}` at the start of `text`: the explicit index, if any, and the text after it.
fn placeholder(text: &str) -> Option<(Option<usize>, &str)> {
    let inner = text.strip_prefix('{')?;
    let end = inner.find('}')?;
    let digits = &inner[..end];
    let index = if digits.is_empty() {
        None
    } else {
        Some(digits.parse().ok()?)
    };
    Some((index, &inner[end + 1..]))
}

/// The Spanish catalog, split by area of the app.
const SPANISH_FILES: &[(&str, &str)] = &[
    ("common", include_str!("../locales/es/common.json")),
    ("settings", include_str!("../locales/es/settings.json")),
    ("workspace", include_str!("../locales/es/workspace.json")),
    ("agent", include_str!("../locales/es/agent.json")),
    ("ai", include_str!("../locales/es/ai.json")),
    ("terminal", include_str!("../locales/es/terminal.json")),
    ("code", include_str!("../locales/es/code.json")),
];

struct Catalog(HashMap<&'static str, &'static str>);

impl Catalog {
    fn parse(files: &[(&str, &'static str)]) -> Catalog {
        let mut entries = HashMap::new();
        for (name, json) in files {
            match serde_json::from_str::<HashMap<String, String>>(json) {
                Ok(file) => entries.extend(file.into_iter().map(|(english, translated)| {
                    let english: &'static str = Box::leak(english.into_boxed_str());
                    let translated: &'static str = Box::leak(translated.into_boxed_str());
                    (english, translated)
                })),
                // The tests check every file, so this only guards against a broken build.
                Err(error) => eprintln!("Ignoring translation file {name}: {error}"),
            }
        }
        Catalog(entries)
    }

    fn get(&self, english: &str) -> Option<&'static str> {
        self.0.get(english).copied()
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
