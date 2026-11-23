//! Settings for the local model server that serves the agent in builds that reach no Warp
//! servers.

use std::cell::RefCell;
use std::collections::HashMap;

use settings::{Setting, ToggleableSetting as _};
use warpui::elements::{CrossAxisAlignment, Flex, MouseStateHandle, ParentElement, Text};
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::ui_components::switch::SwitchStateHandle;
use warpui::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use super::ai_shared::{render_ai_setting_description, render_ai_setting_toggle};
use super::editor_text_colors;
use crate::appearance::Appearance;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions};
use crate::settings::{AISettings, AISettingsChangedEvent, LocalModelEnabled};

/// The settings search query that reveals this section.
pub const LOCAL_MODEL_SETTINGS_SEARCH_QUERY: &str = "local model";

const DESCRIPTION: &str = "Run the agent on a model server on your machine, such as Ollama. \
    Nothing is sent to Warp or anywhere else.";
const INPUT_WIDTH: f32 = 280.;
const LABEL_FONT_SIZE: f32 = 13.;

/// The result of the last attempt to list the server's models.
enum ServerStatus {
    NotChecked,
    Checking,
    Connected,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum LocalModelSettingsAction {
    ToggleEnabled,
    DetectModels,
    SelectModel(String),
}

pub struct LocalModelSettingsView {
    url_editor: ViewHandle<EditorView>,
    context_window_editor: ViewHandle<EditorView>,
    enabled_switch: SwitchStateHandle,
    tooltip_states: RefCell<HashMap<String, MouseStateHandle>>,
    detect_button: MouseStateHandle,
    /// The models the server reported, each with the state of its selection button.
    models: Vec<(String, MouseStateHandle)>,
    status: ServerStatus,
}

impl LocalModelSettingsView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let settings = AISettings::as_ref(ctx);
        let url = settings.local_model_url.value().clone();
        let context_window = settings.local_model_context_window.value().to_string();
        let is_enabled = *settings.local_model_enabled;

        let url_editor = Self::create_editor("http://127.0.0.1:11434", &url, ctx);
        ctx.subscribe_to_view(&url_editor, |me, _, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                me.save_url(ctx);
            }
        });
        let context_window_editor = Self::create_editor("16384", &context_window, ctx);
        ctx.subscribe_to_view(&context_window_editor, |me, _, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                me.save_context_window(ctx);
            }
        });
        ctx.subscribe_to_model(&AISettings::handle(ctx), |_, _, event, ctx| {
            if matches!(
                event,
                AISettingsChangedEvent::LocalModelEnabled { .. }
                    | AISettingsChangedEvent::LocalModelUrl { .. }
                    | AISettingsChangedEvent::LocalModelName { .. }
                    | AISettingsChangedEvent::LocalModelContextWindow { .. }
            ) {
                ctx.notify();
            }
        });

        let mut me = Self {
            url_editor,
            context_window_editor,
            enabled_switch: SwitchStateHandle::default(),
            tooltip_states: RefCell::new(HashMap::new()),
            detect_button: MouseStateHandle::default(),
            models: Vec::new(),
            status: ServerStatus::NotChecked,
        };
        if is_enabled {
            me.detect_models(ctx);
        }
        me
    }

    fn create_editor(
        placeholder: &'static str,
        text: &str,
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<EditorView> {
        let text = text.to_owned();
        ctx.add_typed_action_view(move |ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = SingleLineEditorOptions {
                text: TextOptions {
                    font_size_override: Some(appearance.ui_font_size()),
                    font_family_override: Some(appearance.monospace_font_family()),
                    text_colors_override: Some(editor_text_colors(appearance)),
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text(placeholder, ctx);
            editor.set_buffer_text(&text, ctx);
            editor
        })
    }

    fn save_url(&mut self, ctx: &mut ViewContext<Self>) {
        let url = self
            .url_editor
            .as_ref(ctx)
            .buffer_text(ctx)
            .trim()
            .to_owned();
        if url == *AISettings::as_ref(ctx).local_model_url.value() {
            return;
        }
        AISettings::handle(ctx).update(ctx, |settings, ctx| {
            if let Err(error) = settings.local_model_url.set_value(url, ctx) {
                log::warn!("Failed to save the local model server address: {error:?}");
            }
        });
        self.detect_models(ctx);
    }

    fn save_context_window(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.context_window_editor.as_ref(ctx).buffer_text(ctx);
        match text.trim().parse::<usize>() {
            Ok(window) if window > 0 => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    if let Err(error) = settings.local_model_context_window.set_value(window, ctx) {
                        log::warn!("Failed to save the local model context window: {error:?}");
                    }
                });
            }
            _ => {
                // Restore the saved value instead of keeping an invalid one.
                let saved = AISettings::as_ref(ctx)
                    .local_model_context_window
                    .value()
                    .to_string();
                self.context_window_editor.update(ctx, |editor, ctx| {
                    editor.set_buffer_text(&saved, ctx);
                });
            }
        }
    }

    /// Lists the models installed on the configured server. If no model is selected yet, or the
    /// selected one is not installed, selects the first installed model.
    fn detect_models(&mut self, ctx: &mut ViewContext<Self>) {
        let url = AISettings::as_ref(ctx)
            .local_model_url
            .value()
            .trim()
            .to_owned();
        if url.is_empty() {
            self.status = ServerStatus::Failed("Enter the server address.".to_owned());
            ctx.notify();
            return;
        }
        self.status = ServerStatus::Checking;
        ctx.notify();
        let client = light_llm::OllamaClient::new(url);
        ctx.spawn(
            async move { client.list_models().await },
            |me, result, ctx| {
                match result {
                    Ok(installed) => {
                        me.models = installed
                            .into_iter()
                            .map(|model| (model.name, MouseStateHandle::default()))
                            .collect();
                        me.status = ServerStatus::Connected;
                        let selected = AISettings::as_ref(ctx).local_model_name.value().clone();
                        if !me.models.iter().any(|(name, _)| *name == selected)
                            && let Some((first, _)) = me.models.first()
                        {
                            let first = first.clone();
                            me.select_model(first, ctx);
                        }
                    }
                    Err(error) => {
                        me.models.clear();
                        me.status = ServerStatus::Failed(error.to_string());
                    }
                }
                ctx.notify();
            },
        );
    }

    fn select_model(&mut self, model: String, ctx: &mut ViewContext<Self>) {
        AISettings::handle(ctx).update(ctx, |settings, ctx| {
            if let Err(error) = settings.local_model_name.set_value(model, ctx) {
                log::warn!("Failed to save the local model: {error:?}");
            }
        });
    }

    fn label(text: &str, appearance: &Appearance) -> Box<dyn Element> {
        Text::new(
            text.to_owned(),
            appearance.ui_font_family(),
            LABEL_FONT_SIZE,
        )
        .with_color(appearance.theme().active_ui_text_color().into())
        .finish()
    }

    fn text_input(
        editor: &ViewHandle<EditorView>,
        width: f32,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        appearance
            .ui_builder()
            .text_input(editor.clone())
            .with_style(UiComponentStyles {
                width: Some(width),
                padding: Some(Coords {
                    top: 4.,
                    bottom: 4.,
                    left: 6.,
                    right: 6.,
                }),
                background: Some(appearance.theme().surface_2().into()),
                ..Default::default()
            })
            .build()
            .finish()
    }

    fn button(
        label: String,
        state: &MouseStateHandle,
        action: LocalModelSettingsAction,
        app: &AppContext,
    ) -> Box<dyn Element> {
        Appearance::as_ref(app)
            .ui_builder()
            .button(ButtonVariant::Outlined, state.clone())
            .with_text_label(label)
            .build()
            .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
            .finish()
    }

    fn render_status(&self, appearance: &Appearance) -> Box<dyn Element> {
        let text = match &self.status {
            ServerStatus::NotChecked => String::new(),
            ServerStatus::Checking => "Connecting…".to_owned(),
            ServerStatus::Connected if self.models.is_empty() => {
                "Connected, but no models are installed. Install one, for example with \
                 `ollama pull qwen3:8b`."
                    .to_owned()
            }
            ServerStatus::Connected => format!("Connected · {} models", self.models.len()),
            ServerStatus::Failed(error) => format!("Could not connect: {error}"),
        };
        Self::label(&text, appearance)
    }
}

impl Entity for LocalModelSettingsView {
    type Event = ();
}

impl View for LocalModelSettingsView {
    fn ui_name() -> &'static str {
        "LocalModelSettingsView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let settings = AISettings::as_ref(app);
        let is_enabled = *settings.local_model_enabled;

        let mut column = Flex::column()
            .with_spacing(12.)
            .with_child(render_ai_setting_toggle::<LocalModelEnabled>(
                "Use a local model server",
                LocalModelSettingsAction::ToggleEnabled,
                is_enabled,
                true,
                self.enabled_switch.clone(),
                &self.tooltip_states,
                app,
            ))
            .with_child(render_ai_setting_description(DESCRIPTION, true, app));
        if !is_enabled {
            return column.finish();
        }

        column = column
            .with_child(
                Flex::column()
                    .with_spacing(4.)
                    .with_child(Self::label("Server address", appearance))
                    .with_child(Self::text_input(&self.url_editor, INPUT_WIDTH, app))
                    .finish(),
            )
            .with_child(
                Flex::row()
                    .with_spacing(12.)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(Self::button(
                        "Detect models".to_owned(),
                        &self.detect_button,
                        LocalModelSettingsAction::DetectModels,
                        app,
                    ))
                    .with_child(self.render_status(appearance))
                    .finish(),
            );

        if !self.models.is_empty() {
            let selected = settings.local_model_name.value();
            let mut models = Flex::column()
                .with_spacing(4.)
                .with_child(Self::label("Model", appearance));
            for (name, state) in &self.models {
                let label = if name == selected {
                    format!("✓ {name}")
                } else {
                    name.clone()
                };
                models = models.with_child(Self::button(
                    label,
                    state,
                    LocalModelSettingsAction::SelectModel(name.clone()),
                    app,
                ));
            }
            column = column.with_child(models.finish());
        }

        column
            .with_child(
                Flex::column()
                    .with_spacing(4.)
                    .with_child(Self::label("Context window (tokens)", appearance))
                    .with_child(Self::text_input(&self.context_window_editor, 120., app))
                    .finish(),
            )
            .finish()
    }
}

impl TypedActionView for LocalModelSettingsView {
    type Action = LocalModelSettingsAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            LocalModelSettingsAction::ToggleEnabled => {
                let enabled = AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings.local_model_enabled.toggle_and_save_value(ctx)
                });
                match enabled {
                    Ok(true) => self.detect_models(ctx),
                    Ok(false) => {}
                    Err(error) => log::warn!("Failed to toggle the local model server: {error:?}"),
                }
            }
            LocalModelSettingsAction::DetectModels => self.detect_models(ctx),
            LocalModelSettingsAction::SelectModel(model) => self.select_model(model.clone(), ctx),
        }
    }
}
