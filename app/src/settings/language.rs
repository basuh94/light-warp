use light_i18n::Language;
use settings::macros::define_settings_group;
use settings::{SupportedPlatforms, SyncToCloud};

/// The language of the interface.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "The language of the interface.",
    rename_all = "snake_case"
)]
pub enum LanguagePreference {
    /// The language Windows uses, or English if it is not available.
    #[default]
    System,
    English,
    Spanish,
}

impl LanguagePreference {
    pub const ALL: [LanguagePreference; 3] = [
        LanguagePreference::System,
        LanguagePreference::English,
        LanguagePreference::Spanish,
    ];

    pub fn language(self) -> Language {
        match self {
            LanguagePreference::System => Language::of_system(),
            LanguagePreference::English => Language::English,
            LanguagePreference::Spanish => Language::Spanish,
        }
    }

    /// The name shown in the language picker. Languages keep their own name, so that someone who
    /// picked one they cannot read can still find theirs.
    pub fn label(self) -> &'static str {
        match self {
            LanguagePreference::System => light_i18n::tr("System default"),
            LanguagePreference::English => Language::English.native_name(),
            LanguagePreference::Spanish => Language::Spanish.native_name(),
        }
    }
}

define_settings_group!(LanguageSettings, settings: [
    language: UiLanguage {
        type: LanguagePreference,
        default: LanguagePreference::System,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.language",
        description: "The language of the interface. Takes effect after a restart.",
    },
]);
