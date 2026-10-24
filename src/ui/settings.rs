//! Application-level service configuration, independent of any block.
use std::collections::BTreeMap;

use gpui_kit::base::Disableable;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::*;
use serde_json::Value;
use tradeslope_sdk::runtime::{AccountProfile, BackendRuntime, BackendState, ExtensionSettings};
use tradeslope_sdk::{BackendResult, Field, Schema};

use super::root::RootView;
use crate::extension_manager::InstalledExtension;

pub(super) enum ExtensionAction {
    Refresh,
    Inspect(String),
    Install(std::path::PathBuf),
    SetFrontendEnabled(String, bool),
}

pub(super) struct SettingsSnapshot {
    pub backend: String,
    pub states: Vec<BackendState>,
    pub metadata: ExtensionSettings,
    pub values: Value,
    pub accounts: Vec<AccountProfile>,
    pub active_account: Option<String>,
}

impl SettingsSnapshot {
    pub fn read(runtime: &BackendRuntime, backend: String) -> BackendResult<Self> {
        Ok(Self {
            states: runtime.states(),
            metadata: runtime.settings_metadata(&backend)?,
            values: runtime.settings(&backend)?,
            accounts: runtime.accounts(&backend)?,
            active_account: runtime.active_account(&backend)?,
            backend,
        })
    }
}

pub(super) enum SettingsAction {
    Load,
    Enable(bool),
    Save(Value),
    SaveAccount(AccountProfile),
    RemoveAccount(String),
    SelectAccount(Option<String>),
}

impl SettingsAction {
    pub fn apply(self, runtime: &mut BackendRuntime, backend: &str) -> BackendResult<()> {
        match self {
            Self::Load => Ok(()),
            Self::Enable(enabled) => runtime.set_enabled(backend, enabled),
            Self::Save(value) => runtime.save_settings(backend, value),
            Self::SaveAccount(profile) => {
                let mut accounts = runtime.accounts(backend)?;
                accounts.retain(|account| account.id != profile.id);
                accounts.push(profile);
                runtime.save_accounts(backend, accounts)
            }
            Self::RemoveAccount(id) => {
                let mut accounts = runtime.accounts(backend)?;
                accounts.retain(|account| account.id != id);
                runtime.save_accounts(backend, accounts)
            }
            Self::SelectAccount(id) => runtime.select_account(backend, id),
        }
    }
}

#[derive(Default)]
pub(super) struct AppSettings {
    pub visible: bool,
    pub busy: bool,
    pub backend: String,
    pub states: Vec<BackendState>,
    pub status: String,
    pub packages: Vec<InstalledExtension>,
    pub candidate: Option<tradeslope_package_sdk::Package>,
    package_path: Option<Entity<InputState>>,
    fields: BTreeMap<String, (Field, Entity<InputState>)>,
    accounts: Vec<AccountProfile>,
    active_account: Option<String>,
    account_id: String,
    account_label: Option<Entity<InputState>>,
    account_refs: Option<Entity<InputState>>,
}

impl AppSettings {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {
            package_path: Some(cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Absolute path to an extension package folder")
            })),
            ..Default::default()
        }
    }

    pub fn package_path(&self, cx: &App) -> String {
        self.package_path
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_owned())
            .unwrap_or_default()
    }

    pub fn refresh_state(&mut self, snapshot: SettingsSnapshot) {
        self.states = snapshot.states;
        self.accounts = snapshot.accounts;
        self.active_account = snapshot.active_account;
    }

    pub fn load(&mut self, snapshot: SettingsSnapshot, window: &mut Window, cx: &mut App) {
        self.backend = snapshot.backend;
        self.states = snapshot.states;
        self.fields = snapshot
            .metadata
            .schema
            .fields()
            .into_iter()
            .flatten()
            .map(|(name, field)| {
                let value = snapshot
                    .values
                    .get(name)
                    .map(|value| {
                        if matches!(field.schema, Schema::String | Schema::Enum { .. }) {
                            value.as_str().unwrap_or_default().to_string()
                        } else {
                            value.to_string()
                        }
                    })
                    .unwrap_or_default();
                let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
                (name.clone(), (field.clone(), input))
            })
            .collect();
        self.accounts = snapshot.accounts;
        self.active_account = snapshot.active_account;
        self.edit_account(String::new(), window, cx);
    }

    pub fn edit_account(&mut self, id: String, window: &mut Window, cx: &mut App) {
        self.account_id = id;
        let account = self
            .accounts
            .iter()
            .find(|account| account.id == self.account_id);
        let label = account
            .map(|account| account.label.clone())
            .unwrap_or_default();
        let references = account
            .map(|account| {
                account
                    .env_refs
                    .iter()
                    .map(|(role, name)| format!("{role}={name}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        self.account_label = Some(cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Account label")
                .default_value(label)
        }));
        self.account_refs = Some(cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("api_key=OURBIT_API_KEY, api_secret=OURBIT_API_SECRET")
                .default_value(references)
        }));
    }

    pub fn values(&self, cx: &App) -> Result<Value, String> {
        let mut values = serde_json::Map::new();
        for (name, (field, input)) in &self.fields {
            let text = input.read(cx).value();
            if text.trim().is_empty() && !field.required {
                continue;
            }
            let value = if matches!(field.schema, Schema::String | Schema::Enum { .. }) {
                Value::String(text.to_string())
            } else {
                serde_json::from_str(&text)
                    .map_err(|_| format!("{name}: enter a valid value for this setting"))?
            };
            field
                .schema
                .validate(&value)
                .map_err(|_| format!("{name}: value does not match its declared type"))?;
            values.insert(name.clone(), value);
        }
        Ok(Value::Object(values))
    }

    pub fn profile(&self, cx: &App) -> Result<AccountProfile, String> {
        let label = self
            .account_label
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        if label.is_empty() {
            return Err("Give the account a label.".into());
        }
        let references = self
            .account_refs
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        let mut env_refs = BTreeMap::new();
        for pair in references.split(',').filter(|pair| !pair.trim().is_empty()) {
            let (role, name) = pair.split_once('=').ok_or(
                "Use role=ENVIRONMENT_VARIABLE, separated by commas. Never enter a key or secret.",
            )?;
            let (role, name) = (role.trim(), name.trim());
            if role.is_empty()
                || name.is_empty()
                || !name.bytes().enumerate().all(|(index, byte)| {
                    byte == b'_'
                        || byte.is_ascii_alphabetic()
                        || (index > 0 && byte.is_ascii_digit())
                })
            {
                return Err(
                    "References must be environment-variable names, not credential values.".into(),
                );
            }
            if env_refs.insert(role.into(), name.into()).is_some() {
                return Err("Each credential role may appear only once.".into());
            }
        }
        let id = if self.account_id.is_empty() {
            (1..)
                .map(|index| format!("account-{index}"))
                .find(|id| !self.accounts.iter().any(|profile| &profile.id == id))
                .unwrap()
        } else {
            self.account_id.clone()
        };
        Ok(AccountProfile {
            id,
            label,
            env_refs,
        })
    }

    fn render_extensions(&self, cx: &mut Context<RootView>) -> AnyElement {
        let mut panel = div().flex().flex_col().gap_2()
            .child(div().text_sm().child("Install external providers and cards"))
            .child(div().text_xs().child("Packages are folders containing extension.json. Inspection never runs code. Providers run as trusted local executables when enabled; they are not sandboxed."));
        if let Some(path) = &self.package_path {
            panel = panel
                .child(Input::new(path).small().disabled(self.busy))
                .child(
                    Button::new("inspect-extension")
                        .label("Inspect package")
                        .small()
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let path = this.app_settings.package_path(cx);
                            this.extension_action(ExtensionAction::Inspect(path), cx);
                        })),
                );
        }
        if let Some(candidate) = &self.candidate {
            let path = candidate.root.clone();
            let kind = match &candidate.manifest.kind {
                tradeslope_package_sdk::PackageKind::Provider { .. } => "Provider executable",
                tradeslope_package_sdk::PackageKind::JavaScript { .. } => "Web card",
            };
            panel = panel
                .child(div().text_xs().child(format!(
                    "{} · {} · {}",
                    candidate.manifest.title,
                    kind,
                    candidate.root.display()
                )))
                .child(
                    Button::new("install-extension")
                        .label("Install inspected package")
                        .small()
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.extension_action(ExtensionAction::Install(path.clone()), cx);
                        })),
                );
        }
        for package in &self.packages {
            let id = package.id.clone();
            let provider = package.kind == "Provider executable";
            let enabled = package.enabled;
            let pending = if provider {
                !package.loaded
            } else {
                package.enabled != package.loaded
            };
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .py_2()
                    .child(
                        div()
                            .text_sm()
                            .child(format!("{} · {}", package.title, package.kind)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(package.path.display().to_string()),
                    )
                    .child(div().text_xs().child(if pending {
                        "Restart required to apply this package change"
                    } else if provider && enabled && !package.running {
                        "Enabled but not running — disable and enable to retry"
                    } else if enabled {
                        "Enabled"
                    } else {
                        "Disabled"
                    }))
                    .child(
                        Button::new(SharedString::from(format!("enable-package-{id}")))
                            .label(if provider {
                                if enabled {
                                    "Disable provider"
                                } else {
                                    "Enable trusted provider"
                                }
                            } else if enabled {
                                "Disable card"
                            } else {
                                "Enable card"
                            })
                            .small()
                            .disabled(self.busy || (provider && !package.loaded))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if provider {
                                    this.backend_settings_action(
                                        id.clone(),
                                        SettingsAction::Enable(!enabled),
                                        window,
                                        cx,
                                    );
                                } else {
                                    this.extension_action(
                                        ExtensionAction::SetFrontendEnabled(id.clone(), !enabled),
                                        cx,
                                    );
                                }
                            })),
                    ),
            );
        }
        panel.into_any_element()
    }

    pub fn render(
        &self,
        client: &tradeslope_sdk::BackendClient,
        cx: &mut Context<RootView>,
    ) -> impl IntoElement {
        let choices = client.manifests();
        let selected = self.backend.clone();
        let view = cx.entity().downgrade();
        let enabled = self
            .states
            .iter()
            .find(|state| state.id == self.backend)
            .is_some_and(|state| state.enabled);
        let running = self
            .states
            .iter()
            .find(|state| state.id == self.backend)
            .is_some_and(|state| state.running);
        let backend = self.backend.clone();
        let mut panel = div()
            .id("app-backend-settings")
            .flex().flex_col().gap_2().p_3().max_h(px(420.)).overflow_y_scroll()
            .border_b_1().border_color(cx.theme().border)
            .child(div().text_sm().child("App settings · Extensions, backends & accounts"))
            .child(self.render_extensions(cx))
            .child(div().text_xs().child("Enabled backends run independently of blocks. Configuration is saved in the app database."))
            .child(div().flex().gap_2().items_center()
                .child(Button::new("settings-provider").label(choices.iter().find(|manifest| manifest.id == selected).map(|manifest| manifest.title.clone()).unwrap_or_else(|| "Choose backend".into())).small().disabled(self.busy).dropdown_menu(move |mut menu, _, _| {
                    for manifest in &choices {
                        let id = manifest.id.clone();
                        let view = view.clone();
                        menu = menu.item(PopupMenuItem::new(manifest.title.clone()).checked(id == selected).on_click(move |_, window, cx| {
                            let _ = view.update(cx, |this, cx| this.backend_settings_action(id.clone(), SettingsAction::Load, window, cx));
                        }));
                    }
                    menu
                }))
                .child(div().text_xs().child(if self.busy { "Updating…" } else if running { "Running" } else { "Disabled / unavailable" }))
                .child(Button::new("toggle-backend").label(if enabled { "Disable" } else { "Enable trusted provider" }).small().disabled(self.busy || self.backend.is_empty()).on_click(cx.listener(move |this, _, window, cx| {
                    this.backend_settings_action(backend.clone(), SettingsAction::Enable(!enabled), window, cx);
                }))));
        for (name, (field, input)) in &self.fields {
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().child(name.clone()))
                    .child(Input::new(input).small().disabled(self.busy))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(field.description.clone()),
                    ),
            );
        }
        panel = panel.child(
            Button::new("save-backend-settings")
                .label("Save backend settings")
                .small()
                .disabled(self.busy || self.backend.is_empty())
                .on_click(
                    cx.listener(|this, _, window, cx| this.save_backend_settings(window, cx)),
                ),
        );
        let accounts = self.accounts.clone();
        let active = self.active_account.clone();
        let view = cx.entity().downgrade();
        let view2 = view.clone();
        let backend = self.backend.clone();
        panel = panel.child(div().text_sm().child("Account profiles"))
            .child(div().text_xs().child("Save labels and environment-variable references only, never credentials. Selecting an account does not grant private or trading permission."))
            .child(Button::new("active-backend-account").label(self.active_account.as_ref().and_then(|id| self.accounts.iter().find(|account| &account.id == id)).map(|account| format!("Active: {}", account.label)).unwrap_or_else(|| "Public / no account".into())).small().disabled(self.busy).dropdown_menu(move |mut menu, _, _| {
                let view = view.clone();
                let backend2 = backend.clone();
                menu = menu.item(PopupMenuItem::new("Public / no account").on_click(move |_, window, cx| { let _ = view.update(cx, |this, cx| this.backend_settings_action(backend2.clone(), SettingsAction::SelectAccount(None), window, cx)); }));
                for account in &accounts {
                    let view = view2.clone();
                    let backend = backend.clone();
                    let id = account.id.clone();
                    menu = menu.item(PopupMenuItem::new(account.label.clone()).checked(active.as_ref() == Some(&id)).on_click(move |_, window, cx| { let _ = view.update(cx, |this, cx| this.backend_settings_action(backend.clone(), SettingsAction::SelectAccount(Some(id.clone())), window, cx)); }));
                }
                menu
            }));
        for account in &self.accounts {
            let id = account.id.clone();
            let remove = id.clone();
            let backend = self.backend.clone();
            panel = panel.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("edit-account-{id}")))
                            .label(account.label.clone())
                            .small()
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.app_settings.edit_account(id.clone(), window, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("remove-account-{remove}")))
                            .label("Remove")
                            .small()
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.backend_settings_action(
                                    backend.clone(),
                                    SettingsAction::RemoveAccount(remove.clone()),
                                    window,
                                    cx,
                                )
                            })),
                    ),
            );
        }
        if let (Some(label), Some(references)) = (&self.account_label, &self.account_refs) {
            panel = panel
                .child(Input::new(label).small().disabled(self.busy))
                .child(Input::new(references).small().disabled(self.busy))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("save-account-profile")
                                .label("Save profile")
                                .small()
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.save_account_profile(window, cx)
                                })),
                        )
                        .child(
                            Button::new("new-account-profile")
                                .label("New profile")
                                .small()
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.app_settings.edit_account(String::new(), window, cx);
                                    cx.notify();
                                })),
                        ),
                );
        }
        panel.child(div().text_xs().child(self.status.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::{AppSettings, SettingsSnapshot};
    use serde_json::json;
    use tradeslope_sdk::runtime::{BackendState, ExtensionSettings};

    #[test]
    fn failure_refresh_updates_actual_state_without_discarding_drafts() {
        let mut settings = AppSettings {
            backend: "source".into(),
            account_id: "editing-profile".into(),
            status: "stop failed".into(),
            ..Default::default()
        };
        settings.refresh_state(SettingsSnapshot {
            backend: "source".into(),
            states: vec![BackendState {
                id: "source".into(),
                enabled: false,
                running: false,
            }],
            metadata: ExtensionSettings::default(),
            values: json!({}),
            accounts: vec![],
            active_account: None,
        });
        assert!(!settings.states[0].enabled);
        assert!(!settings.states[0].running);
        assert_eq!(settings.account_id, "editing-profile");
        assert_eq!(settings.status, "stop failed");
    }
}
