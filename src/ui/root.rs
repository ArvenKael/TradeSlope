use super::CardVisibility;
use super::settings::{AppSettings, ExtensionAction, SettingsAction, SettingsSnapshot};
use super::sidebar::sidebar;
use super::source_picker::{SourcePicker, SourcePickerEvent};
use super::tabs::{TabManager, tab_bar};
use super::title_bar::title_bar;
use super::{connections::Connections, native_cards::NativeCards, web_cards::WebCards};
use crate::extension_manager::{ExtensionManager, ExtensionSystem};
use crate::extensions::Extensions;
use crate::page::drag::LiveDrag;
use crate::page::workspace::{PaneId, Workspace};
use crate::page::{
    CardKind, PageId, WorkspacePage,
    builder::{CardViews, workspace_page},
    welcome::welcome_page,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, IconName, Sizable};
use gpui_kit::*;
use tradeslope_sdk::Bindings;

struct ActiveSourcePicker {
    target: (PageId, PaneId),
    view: Entity<SourcePicker>,
    _events: Subscription,
}

pub struct RootView {
    tabs: TabManager,
    pages: Vec<WorkspacePage>,
    sidebar_visible: bool,
    editing_page: bool,
    page_name: Entity<InputState>,
    next_page_id: PageId,
    card_drag: LiveDrag,
    web_cards: WebCards,
    native_cards: NativeCards,
    pub(crate) extensions: Extensions,
    backend: tradeslope_sdk::BackendClient,
    backend_error: Option<String>,
    backend_runtime:
        Option<std::sync::Arc<std::sync::Mutex<tradeslope_sdk::runtime::BackendRuntime>>>,
    extension_manager: Option<std::sync::Arc<ExtensionManager>>,
    pub(super) app_settings: AppSettings,
    mapping_input_observers: Vec<(EntityId, Subscription)>,
    source_picker: Option<ActiveSourcePicker>,
    enabled_backends: std::collections::HashSet<String>,
    pub(super) connections: Connections,
}

impl RootView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ExtensionSystem {
            registry: extensions,
            host,
            runtime: backend_runtime,
            manager: extension_manager,
            error: backend_error,
        } = ExtensionSystem::load();
        let backend = host.client(Default::default());
        let enabled_backends = backend_runtime
            .as_ref()
            .and_then(|runtime| runtime.lock().ok())
            .map(|runtime| {
                runtime
                    .states()
                    .into_iter()
                    .filter(|state| state.enabled && state.running)
                    .map(|state| state.id)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(runtime) = &backend_runtime {
            let _ = runtime.lock().map(|mut runtime| {
                runtime.set_invalidator(Some(backend.invalidator()))?;
                runtime.set_resetter(Some(backend.resetter()))
            });
        }
        Self {
            tabs: TabManager::new(),
            pages: vec![WorkspacePage {
                id: 0,
                name: "Workspace".into(),
                workspace: Workspace::new(),
            }],
            sidebar_visible: true,
            editing_page: false,
            page_name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Page name")
                    .default_value("Workspace")
            }),
            next_page_id: 1,
            card_drag: LiveDrag::default(),
            web_cards: WebCards::default(),
            native_cards: NativeCards::default(),
            extensions,
            backend,
            backend_error,
            backend_runtime,
            extension_manager,
            app_settings: AppSettings::new(window, cx),
            mapping_input_observers: vec![],
            source_picker: None,
            enabled_backends,
            connections: Connections::default(),
        }
    }

    pub(super) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        cx.notify();
    }

    pub(super) fn new_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_page_id;
        self.next_page_id += 1;
        self.pages.push(WorkspacePage {
            id,
            name: format!("Untitled {id}"),
            workspace: Workspace::new(),
        });
        self.editing_page = true;
        self.open_page(id, window, cx);
    }

    pub(super) fn open_page(&mut self, id: PageId, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.open(id);
        self.sync_page_fields(window, cx);
        cx.notify();
    }

    pub(super) fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.select(index);
        self.sync_page_fields(window, cx);
        cx.notify();
    }

    pub(super) fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.close(index);
        self.sync_page_fields(window, cx);
        cx.notify();
    }

    fn active_page(&self) -> Option<&WorkspacePage> {
        let id = self.tabs.active_page()?;
        self.pages.iter().find(|page| page.id == id)
    }

    fn sync_page_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.source_picker = None;
        self.connections.visible = false;
        let name = self
            .active_page()
            .map(|page| page.name.clone())
            .unwrap_or_default();
        self.page_name
            .update(cx, |state, cx| state.set_value(name, window, cx));
        self.connections.target = None;
    }

    fn apply_page_settings(&mut self, cx: &mut Context<Self>) {
        let name = self.page_name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        let id = self.tabs.active_page();
        if let Some(page) = self.pages.iter_mut().find(|page| Some(page.id) == id) {
            page.name = name;
        }
        self.editing_page = false;
        cx.notify();
    }

    fn selected_bindings(&self) -> Bindings {
        self.connections
            .target
            .and_then(|(page, card)| {
                self.pages
                    .iter()
                    .find(|candidate| candidate.id == page)?
                    .workspace
                    .bindings
                    .get(&card)
                    .cloned()
            })
            .unwrap_or_default()
    }

    fn sync_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self
            .active_page()
            .and_then(|page| page.workspace.selected.map(|card| (page, card)));
        let target = selected.map(|(page, card)| (page.id, card));
        let manifest = selected
            .and_then(|(page, card)| page.workspace.kind(card))
            .and_then(|kind| self.extensions.manifest(kind));
        let bindings = selected
            .and_then(|(page, card)| page.workspace.bindings.get(&card))
            .cloned()
            .unwrap_or_default();
        if self.connections.visible && self.connections.target != target {
            self.connections.visible = false;
        }
        self.connections
            .sync(target, manifest, &bindings, &self.backend, window, cx);
        let inputs: Vec<_> = self
            .connections
            .parameters
            .values()
            .map(|draft| draft.input.clone())
            .chain(
                self.connections
                    .raw_parameters
                    .iter()
                    .map(|draft| draft.input.clone()),
            )
            .collect();
        let ids: Vec<_> = inputs.iter().map(|input| input.entity_id()).collect();
        if self
            .mapping_input_observers
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>()
            != ids
        {
            self.mapping_input_observers = inputs
                .into_iter()
                .map(|input| {
                    (
                        input.entity_id(),
                        cx.observe(&input, |_, _, cx| cx.notify()),
                    )
                })
                .collect();
        }
    }

    pub(crate) fn open_bindings(
        &mut self,
        card: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_card(card, cx);
        self.source_picker = None;
        self.app_settings.visible = false;
        self.connections.visible = true;
        self.sync_connections(window, cx);
        cx.notify();
    }

    pub(crate) fn choose_card_source(
        &mut self,
        card: PaneId,
        port: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = self.active_page() else {
            return;
        };
        let Some(manifest) = page
            .workspace
            .kind(card)
            .and_then(|kind| self.extensions.manifest(kind))
        else {
            return;
        };
        let target = (page.id, card);
        let bindings = page
            .workspace
            .bindings
            .get(&card)
            .cloned()
            .unwrap_or_default();
        let expected_card = manifest.id.clone();
        let allowed = self.enabled_backends.clone();
        let picker = cx.new(|cx| {
            let mut picker = SourcePicker::new(
                self.backend.clone(),
                manifest.inputs,
                bindings,
                port,
                window,
                cx,
            );
            picker.set_allowed_backends(Some(allowed), cx);
            picker
        });
        let events = cx.subscribe(&picker, move |this, _, event, cx| {
            match event {
                SourcePickerEvent::Connect { port, binding } => {
                    let current = this
                        .pages
                        .iter()
                        .find(|page| page.id == target.0)
                        .and_then(|page| page.workspace.kind(target.1))
                        .and_then(|kind| this.extensions.manifest(kind));
                    if let Some(manifest) = current
                        && manifest.id == expected_card
                        && let Some(input) = manifest.inputs.iter().find(|input| input.id == *port)
                        && tradeslope_sdk::validate_binding(&this.backend, input, binding).is_ok()
                        && this.enabled_backends.contains(&binding.backend)
                        && let Some(page) = this.pages.iter_mut().find(|page| page.id == target.0)
                    {
                        page.workspace
                            .bindings
                            .entry(target.1)
                            .or_default()
                            .insert(port.clone(), binding.clone());
                    }
                    this.source_picker = None;
                }
                SourcePickerEvent::Cancel => this.source_picker = None,
            }
            cx.notify();
        });
        self.select_card(card, cx);
        self.app_settings.visible = false;
        self.connections.visible = false;
        self.source_picker = Some(ActiveSourcePicker {
            target,
            view: picker,
            _events: events,
        });
        cx.notify();
    }

    pub(crate) fn disconnect_card_source(
        &mut self,
        card: PaneId,
        port: String,
        cx: &mut Context<Self>,
    ) {
        let active = self.tabs.active_page();
        if let Some(page) = self.pages.iter_mut().find(|page| Some(page.id) == active)
            && let Some(bindings) = page.workspace.bindings.get_mut(&card)
        {
            bindings.remove(&port);
        }
        self.source_picker = None;
        cx.notify();
    }

    pub(crate) fn close_card_editors(&mut self, cx: &mut Context<Self>) {
        self.source_picker = None;
        self.connections.visible = false;
        cx.notify();
    }

    pub(super) fn select_input_port(
        &mut self,
        port: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bindings = self.selected_bindings();
        self.connections
            .select_port(port, &bindings, &self.backend, window, cx);
        cx.notify();
    }

    pub(super) fn select_mapping_source(
        &mut self,
        backend: String,
        operation: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connections
            .select_source(backend, operation, &self.backend, window, cx);
        cx.notify();
    }

    pub(super) fn apply_mapping(&mut self, cx: &mut Context<Self>) {
        let Some((page, card)) = self.connections.target else {
            return;
        };
        let Some(port) = self.connections.selected_port().cloned() else {
            return;
        };
        let result = self.connections.binding(cx).and_then(|binding| {
            tradeslope_sdk::validate_binding(&self.backend, &port, &binding)
                .map_err(|error| error.to_string())?;
            Ok(binding)
        });
        match result {
            Ok(binding) => {
                if let Some(page) = self.pages.iter_mut().find(|candidate| candidate.id == page)
                    && page.workspace.kind(card).is_some()
                {
                    page.workspace
                        .bindings
                        .entry(card)
                        .or_default()
                        .insert(port.id.clone(), binding);
                    self.connections.status = format!(
                        "Mapped {} for this block only. No other block was changed.",
                        port.title
                    );
                }
            }
            Err(error) => self.connections.status = format!("Mapping not applied: {error}"),
        }
        cx.notify();
    }

    pub(super) fn remove_mapping(&mut self, cx: &mut Context<Self>) {
        if let Some((page, card)) = self.connections.target
            && let Some(page) = self.pages.iter_mut().find(|candidate| candidate.id == page)
            && let Some(bindings) = page.workspace.bindings.get_mut(&card)
        {
            bindings.remove(&self.connections.port);
        }
        self.connections.status = "Input unmapped. Other inputs and blocks are unchanged.".into();
        cx.notify();
    }

    pub(super) fn extension_action(&mut self, action: ExtensionAction, cx: &mut Context<Self>) {
        if self.app_settings.busy {
            return;
        }
        let Some(manager) = self.extension_manager.clone() else {
            self.app_settings.status = self
                .backend_error
                .clone()
                .unwrap_or_else(|| "Extension storage is unavailable.".into());
            cx.notify();
            return;
        };
        let runtime = self.backend_runtime.clone();
        let loaded_cards: std::collections::HashSet<String> =
            self.extensions.javascript.keys().cloned().collect();
        let clear_candidate = matches!(action, ExtensionAction::Install(_));
        self.app_settings.busy = true;
        let job = cx.background_executor().spawn(async move {
            let result = match action {
                ExtensionAction::Refresh => Ok((None, "Install only trusted local packages. Provider executables require explicit enablement.".to_string())),
                ExtensionAction::Inspect(path) => manager.inspect(&path).map(|package| (Some(package), "Metadata inspected without executing code.".into())),
                ExtensionAction::Install(path) => {
                    let mut registered = loaded_cards.clone();
                    if let Some(runtime) = &runtime {
                        let runtime = runtime.lock().map_err(|_| "Backend runtime is unavailable".to_string());
                        match runtime {
                            Ok(runtime) => {
                                registered.extend(runtime.states().into_iter().map(|state| state.id));
                                manager.install(&path.to_string_lossy(), &registered)
                            }
                            Err(error) => return (Err(error), manager.inventory(None, &loaded_cards)),
                        }
                    } else {
                        manager.install(&path.to_string_lossy(), &registered)
                    }.map(|_| (None, "Package installed disabled. Restart the app to register it, then enable it here.".into()))
                },
                ExtensionAction::SetFrontendEnabled(id, enabled) => manager.set_frontend_enabled(&id, enabled).map(|_| (None, "Card preference saved. Restart the app to apply it.".into())),
            };
            let runtime = runtime.as_ref().and_then(|runtime| runtime.lock().ok());
            let packages = manager.inventory(runtime.as_deref(), &loaded_cards);
            (result, packages)
        });
        cx.spawn(async move |view, cx| {
            let (result, packages) = job.await;
            let _ = view.update(cx, |this, cx| {
                this.app_settings.busy = false;
                if let Ok(packages) = packages {
                    this.app_settings.packages = packages;
                }
                match result {
                    Ok((candidate, status)) => {
                        if candidate.is_some() || clear_candidate {
                            this.app_settings.candidate = candidate;
                        }
                        this.app_settings.status = status;
                    }
                    Err(error) => {
                        this.app_settings.candidate = None;
                        this.app_settings.status = error;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn backend_settings_action(
        &mut self,
        backend: String,
        action: SettingsAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.app_settings.busy {
            return;
        }
        let Some(runtime) = self.backend_runtime.clone() else {
            self.app_settings.status = self
                .backend_error
                .clone()
                .unwrap_or_else(|| "Backend runtime is unavailable.".into());
            cx.notify();
            return;
        };
        self.app_settings.busy = true;
        self.app_settings.status = "Updating backend configuration…".into();
        let manager = self.extension_manager.clone();
        let loaded_cards = self.extensions.javascript.keys().cloned().collect();
        let job = cx.background_executor().spawn(async move {
            let mut runtime = runtime
                .lock()
                .map_err(|_| "Backend runtime is unavailable.".to_string())?;
            let outcome = action
                .apply(&mut runtime, &backend)
                .map_err(|error| error.to_string());
            let snapshot =
                SettingsSnapshot::read(&runtime, backend).map_err(|error| error.to_string());
            let packages =
                manager.and_then(|manager| manager.inventory(Some(&runtime), &loaded_cards).ok());
            Ok::<_, String>((outcome, snapshot, packages))
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = job.await;
            let _ = cx.update(|window, cx| {
                view.update(cx, |this, cx| {
                    this.app_settings.busy = false;
                    let result = result.map(|(outcome, snapshot, packages)| {
                        if let Some(packages) = packages { this.app_settings.packages = packages; }
                        (outcome, snapshot)
                    });
                    match result {
                        Ok((Ok(()), Ok(snapshot))) => {
                            this.enabled_backends = snapshot.states.iter().filter(|state| state.enabled && state.running).map(|state| state.id.clone()).collect();
                            this.app_settings.load(snapshot, window, cx);
                            this.app_settings.status = "Configuration loaded and saved. Account selection does not grant trading access.".into();
                        }
                        Ok((Err(error), snapshot)) => {
                            if let Ok(snapshot) = snapshot {
                                this.enabled_backends = snapshot.states.iter().filter(|state| state.enabled && state.running).map(|state| state.id.clone()).collect();
                                this.app_settings.refresh_state(snapshot);
                            }
                            this.app_settings.status = error;
                        }
                        Ok((Ok(()), Err(error))) => this.app_settings.status = error,
                        Err(error) => this.app_settings.status = error,
                    }
                    cx.notify();
                })
            });
        }).detach();
        cx.notify();
    }

    pub(super) fn save_backend_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.app_settings.values(cx) {
            Ok(values) => self.backend_settings_action(
                self.app_settings.backend.clone(),
                SettingsAction::Save(values),
                window,
                cx,
            ),
            Err(error) => {
                self.app_settings.status = error;
                cx.notify();
            }
        }
    }

    pub(super) fn save_account_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.app_settings.profile(cx) {
            Ok(profile) => self.backend_settings_action(
                self.app_settings.backend.clone(),
                SettingsAction::SaveAccount(profile),
                window,
                cx,
            ),
            Err(error) => {
                self.app_settings.status = error;
                cx.notify();
            }
        }
    }

    pub(crate) fn add_card(&mut self, cx: &mut Context<Self>) {
        let id = self.tabs.active_page();
        if let Some(page) = self.pages.iter_mut().find(|page| Some(page.id) == id) {
            page.workspace.add_auto(cx);
        }
        cx.notify();
    }

    pub(crate) fn begin_card_drag(&mut self, cx: &mut Context<Self>) {
        self.card_drag = LiveDrag::default();
        cx.notify();
    }

    pub(crate) fn drag_card(
        &mut self,
        page_id: PageId,
        source: PaneId,
        target: PaneId,
        pointer: Point<Pixels>,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.tabs.active_page() != Some(page_id)
            || source == target
            || !bounds.contains(&pointer)
            || bounds.size.width <= px(0.)
            || bounds.size.height <= px(0.)
        {
            return;
        }
        let relative = pointer - bounds.origin;
        let Some((direction, before)) = self.card_drag.placement(
            target,
            (f32::from(pointer.x), f32::from(pointer.y)),
            (
                relative.x / bounds.size.width,
                relative.y / bounds.size.height,
            ),
        ) else {
            return;
        };
        if let Some(page) = self.pages.iter_mut().find(|page| page.id == page_id) {
            page.workspace
                .move_card(source, target, direction, before, cx);
        }
        cx.notify();
    }

    pub(crate) fn select_card(&mut self, card: PaneId, cx: &mut Context<Self>) {
        let id = self.tabs.active_page();
        if let Some(page) = self.pages.iter_mut().find(|page| Some(page.id) == id) {
            page.workspace.select(card);
        }
        cx.notify();
    }

    pub(crate) fn choose_card(
        &mut self,
        card: PaneId,
        kind: Option<CardKind>,
        cx: &mut Context<Self>,
    ) {
        self.source_picker = None;
        self.connections.visible = false;
        let id = self.tabs.active_page();
        if let Some(page) = self.pages.iter_mut().find(|page| Some(page.id) == id) {
            page.workspace.choose(card, kind);
        }
        self.connections.target = None;
        cx.notify();
    }

    pub(crate) fn remove_card(&mut self, card: PaneId, cx: &mut Context<Self>) {
        if self
            .source_picker
            .as_ref()
            .is_some_and(|picker| picker.target.1 == card)
        {
            self.source_picker = None;
        }
        self.connections.visible = false;
        let id = self.tabs.active_page();
        if let Some(page) = self.pages.iter_mut().find(|page| Some(page.id) == id) {
            page.workspace.remove(card);
        }
        cx.notify();
    }

    fn render_page_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .child(div().w(px(220.)).child(Input::new(&self.page_name).small()))
            .child(
                Button::new("apply-page")
                    .ghost()
                    .small()
                    .label("Apply")
                    .on_click(cx.listener(|this, _, _, cx| this.apply_page_settings(cx))),
            )
    }

    fn render_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden();
        if self.app_settings.visible {
            content = content.child(self.app_settings.render(&self.backend, cx));
            content = content.children(
                self.extensions
                    .errors
                    .iter()
                    .chain(self.backend_error.iter())
                    .map(|error| div().px_3().py_1().text_xs().child(error.clone())),
            );
        }
        if let Some(page) = self.active_page() {
            if self.editing_page {
                content = content.child(self.render_page_settings(cx));
            }
            content = content.child(workspace_page(
                page,
                &CardViews {
                    web: &self.web_cards,
                    native: &self.native_cards,
                    extensions: &self.extensions,
                    backend: &self.backend,
                    picker: self
                        .source_picker
                        .as_ref()
                        .map(|picker| (picker.target, picker.view.clone().into())),
                    advanced: &self.connections,
                },
                cx,
            ));
        } else {
            content = content.child(welcome_page());
        }
        content
    }

    fn render_workspace(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(32.))
                    .flex_shrink_0()
                    .bg(cx.theme().tab_bar)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(tab_bar(&self.tabs, &self.pages, cx)),
                    )
                    .child(
                        Button::new("app-settings")
                            .label("App settings")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.app_settings.visible = !this.app_settings.visible;
                                if this.app_settings.visible {
                                    this.source_picker = None;
                                    this.connections.visible = false;
                                    this.extension_action(ExtensionAction::Refresh, cx);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("edit-page")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Settings2)
                            .tooltip("Page name")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editing_page = !this.editing_page;
                                cx.notify();
                            })),
                    ),
            )
            .child(self.render_page(cx))
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_connections(window, cx);
        let mut editing = std::collections::HashSet::new();
        if let Some(picker) = &self.source_picker {
            editing.insert(picker.target);
        }
        if self.connections.visible
            && let Some(target) = self.connections.target
        {
            editing.insert(target);
        }
        let visibility = CardVisibility {
            active_page: self.tabs.active_page(),
            editing,
        };
        self.web_cards.sync(
            &self.pages,
            &visibility,
            &self.extensions,
            &self.backend,
            window,
            cx,
        );
        self.native_cards.sync(
            &self.pages,
            &visibility,
            &self.extensions,
            &self.backend,
            window,
            cx,
        );
        let mut layout = div().flex().flex_1().min_h_0();
        if self.sidebar_visible {
            layout = layout.child(sidebar(&self.pages, self.tabs.active_page(), cx));
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .text_size(px(13.))
            .text_color(cx.theme().foreground)
            .child(title_bar(
                self.active_page().map(|page| page.name.as_str()),
                self.sidebar_visible,
                cx,
            ))
            .child(layout.child(self.render_workspace(cx)))
    }
}
