/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Estado do navegador: janela, abas, a WebView da interface (chrome) e o roteamento
//! de entrada/pintura entre elas.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use euclid::{Point2D, Rect, Scale, Size2D};
use serde::Deserialize;
use rumen_engine::{
    ConsoleLogLevel, CreateNewWebViewRequest, EmbedderControl, EmbedderControlId, EventLoopWaker,
    FilePicker, Cursor, DevicePoint, InputEvent, LoadStatus,
    MouseButton, MouseButtonAction, MouseButtonEvent, MouseLeftViewportEvent, MouseMoveEvent,
    OffscreenRenderingContext, RenderingContext, Servo, Theme, WebView, WebViewBuilder,
    WebViewDelegate, WheelDelta, WheelEvent, WheelMode, WindowRenderingContext,
};
use url::Url;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey as WinitNamedKey};
use winit::window::{CursorIcon, Fullscreen, Window};

use crate::control::{self, ControlCmd};
use crate::filepicker::{self, PickResult};
use crate::keyutils::keyboard_event_from_winit;
use crate::mode::{self, Mode};
use crate::popup::{Layout, Popup};
use crate::resources::Resources;

/// Altura da interface (abas + barra de endereço) em pixels CSS.
const UI_HEIGHT_CSS: f32 = 76.0;
const SEARCH_URL: &str = "https://duckduckgo.com/html/?q=";
const ZOOM_STEPS: [f32; 13] = [
    0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 2.0, 3.0,
];

/// Mensagens enviadas pela interface HTML via `console.log("RUMEN:{json}")`.
#[derive(Deserialize, Debug)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum UiMessage {
    UiReady,
    NewTab,
    CloseTab { index: usize },
    SelectTab { index: usize },
    Navigate { input: String },
    Back,
    Forward,
    Reload,
    Stop,
    Home,
    FocusContent,
    PopupReady,
    PopupResult {
        ok: bool,
        #[serde(default)]
        value: Option<serde_json::Value>,
    },
}

/// Comandos são enfileirados e executados fora dos callbacks do Servo para evitar
/// reentrância no motor.
#[derive(Debug)]
enum Command {
    Ui(UiMessage),
    NewTab(Option<Url>),
    CloseTab(usize),
    SelectTab(usize),
    Zoom(i32),
    FocusOmnibox,
    ShowPopup,
    ClosePopup,
}

pub struct Browser {
    pub window: Window,
    pub servo: Servo,
    rendering_context: Rc<WindowRenderingContext>,
    ui_context: Rc<OffscreenRenderingContext>,
    content_context: Rc<OffscreenRenderingContext>,
    popup_context: Rc<OffscreenRenderingContext>,
    popup_view: RefCell<Option<WebView>>,
    popup_ready: Cell<bool>,
    popup: RefCell<Option<Popup>>,
    resources: Resources,
    ui: RefCell<Option<WebView>>,
    tabs: RefCell<Vec<WebView>>,
    active: Cell<usize>,
    focus_ui: Cell<bool>,
    ui_ready: Cell<bool>,
    ui_dirty: Cell<bool>,
    title_dirty: Cell<bool>,
    commands: RefCell<Vec<Command>>,
    cursor_pos: Cell<PhysicalPosition<f64>>,
    pressed_in_ui: Cell<Option<bool>>,
    modifiers: Cell<ModifiersState>,
    quit: Cell<bool>,
    weak_self: RefCell<Weak<Browser>>,
    mode: Mode,
    waker: Box<dyn EventLoopWaker>,
    file_pick: RefCell<Option<(FilePicker, std::sync::mpsc::Receiver<PickResult>)>>,
    /// Favicons já codificados como `data:image/png;base64,...`, por aba.
    favicons: RefCell<std::collections::HashMap<rumen_engine::WebViewId, String>>,
    control: RefCell<Option<std::sync::mpsc::Receiver<control::Request>>>,
    session_dirty: Cell<bool>,
    last_session_save: Cell<std::time::Instant>,
    occluded: Cell<bool>,
}

impl Browser {
    pub fn new(
        window: Window,
        servo: Servo,
        rendering_context: Rc<WindowRenderingContext>,
        resources: Resources,
        initial_urls: Vec<Url>,
        mode: Mode,
        waker: Box<dyn EventLoopWaker>,
    ) -> Rc<Self> {
        let size = window.inner_size();
        let ui_h = ui_height_px(window.scale_factor());
        let ui_context = Rc::new(rendering_context.offscreen_context(PhysicalSize::new(
            size.width.max(1),
            ui_h.max(1),
        )));
        let content_context = Rc::new(rendering_context.offscreen_context(PhysicalSize::new(
            size.width.max(1),
            size.height.saturating_sub(ui_h).max(1),
        )));

        let popup_context =
            Rc::new(rendering_context.offscreen_context(PhysicalSize::new(240, 120)));
        let browser = Rc::new(Self {
            window,
            servo,
            rendering_context,
            ui_context,
            content_context,
            popup_context,
            popup_view: RefCell::new(None),
            popup_ready: Cell::new(false),
            popup: RefCell::new(None),
            resources,
            ui: RefCell::new(None),
            tabs: RefCell::new(Vec::new()),
            active: Cell::new(0),
            focus_ui: Cell::new(false),
            ui_ready: Cell::new(false),
            ui_dirty: Cell::new(true),
            title_dirty: Cell::new(true),
            commands: RefCell::new(Vec::new()),
            cursor_pos: Cell::new(PhysicalPosition::new(0.0, 0.0)),
            pressed_in_ui: Cell::new(None),
            modifiers: Cell::new(ModifiersState::empty()),
            quit: Cell::new(false),
            weak_self: RefCell::new(Weak::new()),
            mode,
            waker,
            file_pick: RefCell::new(None),
            favicons: RefCell::default(),
            control: RefCell::new(None),
            session_dirty: Cell::new(false),
            last_session_save: Cell::new(std::time::Instant::now()),
            occluded: Cell::new(false),
        });
        *browser.weak_self.borrow_mut() = Rc::downgrade(&browser);

        let ui = WebViewBuilder::new(&browser.servo, browser.ui_context.clone())
            .url(browser.resources.ui_url())
            .hidpi_scale_factor(browser.scale())
            .delegate(browser.clone())
            .build();
        ui.notify_theme_change(Theme::Dark);
        ui.show();
        *browser.ui.borrow_mut() = Some(ui);

        let popup_view = WebViewBuilder::new(&browser.servo, browser.popup_context.clone())
            .url(browser.resources.popup_url())
            .hidpi_scale_factor(browser.scale())
            .delegate(browser.clone())
            .build();
        popup_view.notify_theme_change(Theme::Dark);
        popup_view.show();
        *browser.popup_view.borrow_mut() = Some(popup_view);

        if initial_urls.is_empty() {
            browser.open_tab(None);
        } else {
            for url in initial_urls {
                browser.open_tab(Some(url));
            }
            browser.select_tab(0);
        }
        browser
    }

    fn scale(&self) -> Scale<f32, rumen_engine::DeviceIndependentPixel, rumen_engine::DevicePixel> {
        Scale::new(self.window.scale_factor() as f32)
    }

    fn ui_height(&self) -> u32 {
        ui_height_px(self.window.scale_factor())
    }

    fn delegate(&self) -> Rc<dyn WebViewDelegate> {
        self.weak_self.borrow().upgrade().expect("browser vivo")
    }

    fn is_popup(&self, webview: &WebView) -> bool {
        self.popup_view
            .borrow()
            .as_ref()
            .is_some_and(|p| p.id() == webview.id())
    }

    fn is_ui(&self, webview: &WebView) -> bool {
        self.ui
            .borrow()
            .as_ref()
            .is_some_and(|ui| ui.id() == webview.id())
    }

    pub fn active_tab(&self) -> Option<WebView> {
        self.tabs.borrow().get(self.active.get()).cloned()
    }

    pub fn should_quit(&self) -> bool {
        self.quit.get()
    }

    pub fn session_urls(&self) -> Vec<String> {
        self.tabs
            .borrow()
            .iter()
            .filter_map(|t| t.url())
            .filter(|u| !self.resources.is_internal(u))
            .map(|u| u.to_string())
            .collect()
    }

    // ---------------------------------------------------------------- abas

    fn open_tab(&self, url: Option<Url>) -> WebView {
        let url = url.unwrap_or_else(|| self.resources.newtab_url());
        let webview = WebViewBuilder::new(&self.servo, self.content_context.clone())
            .url(url)
            .hidpi_scale_factor(self.scale())
            .delegate(self.delegate())
            .build();
        self.adopt_tab(webview.clone());
        webview
    }

    fn adopt_tab(&self, webview: WebView) {
        let index = {
            let mut tabs = self.tabs.borrow_mut();
            tabs.push(webview);
            tabs.len() - 1
        };
        self.select_tab(index);
    }

    fn select_tab(&self, index: usize) {
        self.close_popup();
        let tabs = self.tabs.borrow();
        if index >= tabs.len() {
            return;
        }
        for (i, tab) in tabs.iter().enumerate() {
            if i == index {
                tab.show();
                tab.set_focused(true);
            } else {
                tab.hide();
                tab.set_focused(false);
            }
        }
        self.active.set(index);
        self.focus_ui.set(false);
        self.mark_dirty();
    }

    fn close_tab(&self, index: usize) {
        let len = {
            let mut tabs = self.tabs.borrow_mut();
            if index >= tabs.len() {
                return;
            }
            tabs.remove(index);
            tabs.len()
        };
        if len == 0 {
            // O RUMEN nunca fecha sozinho: sem abas, abre uma nova aba em branco.
            self.open_tab(None);
            return;
        }
        let active = self.active.get();
        let new_active = if index < active || active >= len {
            active.saturating_sub(1)
        } else {
            active
        };
        self.select_tab(new_active.min(len - 1));
    }

    fn mark_dirty(&self) {
        self.session_dirty.set(true);
        self.ui_dirty.set(true);
        self.title_dirty.set(true);
        self.window.request_redraw();
    }

    // ------------------------------------------------------- comandos/UI

    /// Executa comandos pendentes e sincroniza a interface. Chamado após cada
    /// `spin_event_loop`, fora de callbacks do Servo.
    pub fn flush(&self) {
        self.poll_file_pick();
        self.poll_control();
        self.autosave_session();
        let commands: Vec<Command> = std::mem::take(&mut *self.commands.borrow_mut());
        for command in commands {
            self.run(command);
        }
        if self.ui_dirty.replace(false) && self.ui_ready.get() {
            self.push_ui_state();
        }
        if self.title_dirty.replace(false) {
            let title = self
                .active_tab()
                .and_then(|t| t.page_title())
                .filter(|t| !t.is_empty());
            let suffix = self.mode.title_suffix();
            self.window.set_title(&match title {
                Some(t) => format!("{t} — {suffix}"),
                None => suffix.to_owned(),
            });
        }
    }

    fn run(&self, command: Command) {
        match command {
            Command::NewTab(url) => {
                let blank = url.is_none();
                self.open_tab(url);
                if blank {
                    self.focus_omnibox();
                }
            },
            Command::CloseTab(i) => self.close_tab(i),
            Command::SelectTab(i) => self.select_tab(i),
            Command::Zoom(direction) => self.zoom(direction),
            Command::FocusOmnibox => self.focus_omnibox(),
            Command::ShowPopup => self.push_popup(),
            Command::ClosePopup => self.close_popup(),
            Command::Ui(message) => self.handle_ui_message(message),
        }
    }

    fn handle_ui_message(&self, message: UiMessage) {
        let tab = self.active_tab();
        match message {
            UiMessage::UiReady => {
                self.ui_ready.set(true);
                self.ui_dirty.set(true);
                if tab
                    .as_ref()
                    .and_then(|t| t.url())
                    .is_some_and(|u| self.resources.is_newtab(&u))
                {
                    self.focus_omnibox();
                }
            },
            UiMessage::NewTab => self.run(Command::NewTab(None)),
            UiMessage::CloseTab { index } => self.close_tab(index),
            UiMessage::SelectTab { index } => self.select_tab(index),
            UiMessage::Navigate { input } => {
                if let (Some(tab), Some(url)) = (tab, self.resolve_input(&input)) {
                    tab.load(url);
                    self.focus_content();
                }
            },
            UiMessage::Back => {
                if let Some(tab) = tab.filter(|t| t.can_go_back()) {
                    tab.go_back(1);
                }
            },
            UiMessage::Forward => {
                if let Some(tab) = tab.filter(|t| t.can_go_forward()) {
                    tab.go_forward(1);
                }
            },
            UiMessage::Reload => {
                if let Some(tab) = tab {
                    tab.reload();
                }
            },
            UiMessage::Stop => {
                // Servo 0.7 não expõe uma API de "parar carregamento".
                self.mark_dirty();
            },
            UiMessage::Home => {
                if let Some(tab) = tab {
                    tab.load(self.resources.newtab_url());
                }
            },
            UiMessage::FocusContent => self.focus_content(),
            UiMessage::PopupReady => {
                self.popup_ready.set(true);
                self.push_popup();
            },
            UiMessage::PopupResult { ok, value } => {
                if let Some(popup) = self.popup.borrow_mut().take() {
                    popup.resolve(ok, value);
                }
                self.after_popup_closed();
            },
        }
        self.mark_dirty();
    }

    /// Converte o texto da omnibox em URL: endereço explícito, domínio ou busca.
    fn resolve_input(&self, input: &str) -> Option<Url> {
        let input = input.trim();
        if input.is_empty() {
            return None;
        }
        if let Ok(url) = Url::parse(input) {
            match url.scheme() {
                "http" | "https" | "file" | "data" | "about" => return Some(web3_gateway(url)),
                // Web3: conteúdo IPFS/IPNS via gateway com isolamento por subdomínio.
                "ipfs" | "ipns" => {
                    let id = url.host_str().unwrap_or_default();
                    let path = url.path();
                    return Url::parse(&format!("https://{id}.{}.dweb.link{path}", url.scheme())).ok();
                },
                _ => {},
            }
        }
        let looks_like_host = !input.contains(char::is_whitespace) &&
            (input.contains('.') || input.starts_with("localhost")) &&
            !input.ends_with('.');
        if looks_like_host {
            let host = input.split('/').next().unwrap_or_default();
            // Sites .onion quase sempre servem HTTP puro; o Tor já criptografa ponta a ponta.
            let scheme = if input.starts_with("localhost") ||
                input.starts_with("127.") ||
                host.ends_with(".onion")
            {
                "http"
            } else {
                "https"
            };
            if let Ok(url) = Url::parse(&format!("{scheme}://{input}")) {
                return Some(web3_gateway(url));
            }
        }
        let query: String = url::form_urlencoded::byte_serialize(input.as_bytes()).collect();
        Url::parse(&format!("{SEARCH_URL}{query}")).ok()
    }

    fn push_ui_state(&self) {
        let Some(ui) = self.ui.borrow().clone() else {
            return;
        };
        let tabs: Vec<serde_json::Value> = self
            .tabs
            .borrow()
            .iter()
            .map(|t| {
                let url = t.url();
                let internal = url.as_ref().is_some_and(|u| self.resources.is_internal(u));
                let display_url = match &url {
                    Some(u) if !internal => u.to_string(),
                    _ => String::new(),
                };
                let title = t
                    .page_title()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| if internal { "Nova aba".into() } else { display_url.clone() });
                let favicon = self.favicons.borrow().get(&t.id()).cloned();
                serde_json::json!({
                    "favicon": favicon,
                    "title": title,
                    "url": display_url,
                    "display_url": display_url,
                    "loading": t.load_status() != LoadStatus::Complete,
                    "can_back": t.can_go_back(),
                    "can_fwd": t.can_go_forward(),
                    "zoom": t.page_zoom(),
                })
            })
            .collect();
        let state = serde_json::json!({
            "tabs": tabs, "active": self.active.get(), "mode": self.mode.label(),
        });
        ui.evaluate_javascript(format!("window.rumen && rumen.update({state})"), |_| {});
    }

    fn focus_omnibox(&self) {
        self.focus_ui.set(true);
        if let Some(ui) = self.ui.borrow().as_ref() {
            ui.set_focused(true);
            ui.evaluate_javascript("window.rumen && rumen.focusOmnibox()", |_| {});
        }
        if let Some(tab) = self.active_tab() {
            tab.set_focused(false);
        }
    }

    fn focus_content(&self) {
        self.focus_ui.set(false);
        if let Some(ui) = self.ui.borrow().as_ref() {
            ui.set_focused(false);
        }
        if let Some(tab) = self.active_tab() {
            tab.set_focused(true);
        }
    }

    pub fn attach_control(&self, rx: std::sync::mpsc::Receiver<control::Request>) {
        *self.control.borrow_mut() = Some(rx);
    }

    /// Grava a sessão (abas abertas) logo após cada mudança, no máximo a cada 2 s, para que
    /// nada se perca mesmo se o processo morrer.
    fn autosave_session(&self) {
        if !self.mode.persistent() || !self.session_dirty.get() {
            return;
        }
        if self.last_session_save.get().elapsed() < std::time::Duration::from_secs(2) {
            self.window.request_redraw(); // garante nova passada para salvar depois
            return;
        }
        self.save_session();
    }

    pub fn save_session(&self) {
        if !self.mode.persistent() {
            return;
        }
        self.session_dirty.set(false);
        self.last_session_save.set(std::time::Instant::now());
        let path = data_dir().join("session.json");
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(&self.session_urls()).unwrap_or_default();
        // Escrita atômica: nunca deixa um session.json pela metade.
        if std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
            tracing::warn!("não foi possível salvar a sessão em {path:?}");
        }
    }

    fn poll_control(&self) {
        loop {
            let request = match self.control.borrow().as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(r)) => r,
                _ => return,
            };
            self.handle_control(request);
        }
    }

    fn tab_at(&self, index: Option<usize>) -> Option<WebView> {
        match index {
            Some(i) => self.tabs.borrow().get(i).cloned(),
            None => self.active_tab(),
        }
    }

    fn handle_control(&self, request: control::Request) {
        use serde_json::json;
        match &request.cmd {
            ControlCmd::Ping => request.ok(json!({
                "name": "RUMEN", "version": env!("CARGO_PKG_VERSION"),
                "engine": rumen_engine::UPSTREAM, "mode": self.mode.label(),
                "tabs": self.tabs.borrow().len(),
            })),
            ControlCmd::Tabs => {
                let active = self.active.get();
                let tabs: Vec<_> = self.tabs.borrow().iter().enumerate().map(|(i, t)| json!({
                    "index": i, "active": i == active,
                    "title": t.page_title().unwrap_or_default(),
                    "url": t.url().map(|u| u.to_string()).unwrap_or_default(),
                    "loading": t.load_status() != LoadStatus::Complete,
                })).collect();
                request.ok(json!(tabs));
            },
            ControlCmd::Open { url, new_tab } => match self.resolve_input(url) {
                Some(url) => {
                    let shown = url.to_string();
                    match (*new_tab, self.active_tab()) {
                        (false, Some(tab)) => tab.load(url),
                        _ => {
                            self.open_tab(Some(url));
                        },
                    }
                    self.mark_dirty();
                    request.ok(json!({ "opened": shown, "index": self.active.get() }));
                },
                None => request.err("URL vazia"),
            },
            ControlCmd::Select { index } => {
                if *index < self.tabs.borrow().len() {
                    self.select_tab(*index);
                    request.ok(json!({ "active": index }));
                } else {
                    request.err("aba inexistente");
                }
            },
            ControlCmd::Close { index } => {
                if *index < self.tabs.borrow().len() {
                    self.close_tab(*index);
                    request.ok(json!({ "tabs": self.tabs.borrow().len() }));
                } else {
                    request.err("aba inexistente");
                }
            },
            ControlCmd::Back => {
                self.handle_ui_message(UiMessage::Back);
                request.ok(json!(true));
            },
            ControlCmd::Forward => {
                self.handle_ui_message(UiMessage::Forward);
                request.ok(json!(true));
            },
            ControlCmd::Reload => {
                self.handle_ui_message(UiMessage::Reload);
                request.ok(json!(true));
            },
            ControlCmd::Eval { js, index } => self.eval_for(request_js(js), *index, &request),
            ControlCmd::Text { index } => self.eval_for(
                "document.body ? document.body.innerText : ''".to_owned(),
                *index,
                &request,
            ),
            ControlCmd::ReloadUi { dir } => {
                let applied = match dir {
                    Some(d) => match self.resources.apply_dir(std::path::Path::new(d)) {
                        Ok(files) => files,
                        Err(e) => return request.err(format!("falha ao copiar de {d}: {e}")),
                    },
                    None => Vec::new(),
                };
                self.ui_ready.set(false);
                self.popup_ready.set(false);
                self.close_popup();
                if let Some(ui) = self.ui.borrow().as_ref() {
                    ui.reload();
                }
                if let Some(view) = self.popup_view.borrow().as_ref() {
                    view.reload();
                }
                self.mark_dirty();
                request.ok(json!({ "reloaded": true, "applied": applied }));
            },
        }
    }

    fn eval_for(&self, js: String, index: Option<usize>, request: &control::Request) {
        let Some(tab) = self.tab_at(index) else {
            return request.err("aba inexistente");
        };
        let reply = request.reply.clone();
        tab.evaluate_javascript(js, move |result| {
            let response = match result {
                Ok(v) => serde_json::json!({ "ok": true, "result": control::js_to_json(v) }),
                Err(e) => serde_json::json!({ "ok": false, "error": format!("{e:?}") }),
            };
            let _ = reply.send(response);
        });
    }

    fn poll_file_pick(&self) {
        let done = match self.file_pick.borrow().as_ref() {
            Some((_, rx)) => rx.try_recv().ok(),
            None => None,
        };
        let Some(result) = done else { return };
        let Some((mut picker, _)) = self.file_pick.borrow_mut().take() else { return };
        match result {
            Some(paths) => {
                picker.select(&paths);
                picker.submit();
            },
            None => picker.dismiss(),
        }
    }

    fn popup_layout(&self) -> Layout {
        let size = self.window.inner_size();
        Layout {
            scale: self.window.scale_factor() as f32,
            ui_h: self.ui_height() as i32,
            win_w: size.width as i32,
            win_h: size.height as i32,
            origin: self
                .active_tab()
                .and_then(|t| t.url())
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default(),
        }
    }

    /// Envia o popup pendente para `popup.html` e dá foco a ele.
    fn push_popup(&self) {
        let (payload, rect) = match self.popup.borrow().as_ref() {
            Some(p) => (p.payload.clone(), p.rect),
            None => return,
        };
        let Some(view) = self.popup_view.borrow().clone() else { return };
        view.resize(PhysicalSize::new(rect.w as u32, rect.h as u32));
        if self.popup_ready.get() {
            view.evaluate_javascript(format!("rumenPopup.show({payload})"), |_| {});
        }
        view.set_focused(true);
        self.window.request_redraw();
    }

    fn close_popup(&self) {
        if let Some(popup) = self.popup.borrow_mut().take() {
            popup.cancel();
            self.after_popup_closed();
        }
    }

    fn after_popup_closed(&self) {
        if let Some(view) = self.popup_view.borrow().as_ref() {
            view.set_focused(false);
        }
        if self.focus_ui.get() {
            if let Some(ui) = self.ui.borrow().as_ref() {
                ui.set_focused(true);
            }
        } else if let Some(tab) = self.active_tab() {
            tab.set_focused(true);
        }
        self.window.request_redraw();
    }

    fn popup_rect(&self) -> Option<crate::popup::WinRect> {
        self.popup.borrow().as_ref().map(|p| p.rect)
    }

    fn zoom(&self, direction: i32) {
        let Some(tab) = self.active_tab() else { return };
        let current = tab.page_zoom();
        let next = match direction {
            0 => 1.0,
            d if d > 0 => *ZOOM_STEPS.iter().find(|z| **z > current + 0.001).unwrap_or(&3.0),
            _ => *ZOOM_STEPS
                .iter()
                .rev()
                .find(|z| **z < current - 0.001)
                .unwrap_or(&0.25),
        };
        tab.set_page_zoom(next);
        self.mark_dirty();
    }

    // ------------------------------------------------------------ pintura

    /// Janela escondida (outra mesa, minimizada): não pinta, mas o motor e o controle seguem.
    pub fn set_occluded(&self, hidden: bool) {
        self.occluded.set(hidden);
        if !hidden {
            self.window.request_redraw();
        }
    }

    pub fn paint(&self) {
        if self.occluded.get() {
            return;
        }
        let _ = self.rendering_context.make_current();
        self.rendering_context.prepare_for_rendering();

        let size = self.window.inner_size();
        let ui_h = self.ui_height().min(size.height) as i32;
        let (w, h) = (size.width as i32, size.height as i32);

        if let Some(ui) = self.ui.borrow().as_ref() {
            ui.paint();
        }
        if let Some(tab) = self.active_tab() {
            tab.paint();
        }
        let popup_rect = self.popup_rect();
        if popup_rect.is_some() {
            if let Some(view) = self.popup_view.borrow().as_ref() {
                view.paint();
            }
        }

        let gl = self.rendering_context.glow_gl_api();
        // OpenGL tem origem no canto inferior esquerdo: a UI fica no topo da janela.
        // A UI precisa ser copiada antes do conteúdo: na ordem inversa o conteúdo some.
        if let Some(blit) = self.ui_context.render_to_parent_callback() {
            blit(&gl, Rect::new(Point2D::new(0, h - ui_h), Size2D::new(w, ui_h)));
        }
        if let Some(blit) = self.content_context.render_to_parent_callback() {
            blit(&gl, Rect::new(Point2D::new(0, 0), Size2D::new(w, h - ui_h)));
        }
        if let (Some(r), Some(blit)) = (popup_rect, self.popup_context.render_to_parent_callback()) {
            blit(&gl, Rect::new(Point2D::new(r.x, h - r.y - r.h), Size2D::new(r.w, r.h)));
        }
        self.rendering_context.present();
    }

    pub fn resize(&self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.close_popup();
        let ui_h = self.ui_height();
        self.rendering_context.resize(size);
        if let Some(ui) = self.ui.borrow().as_ref() {
            ui.resize(PhysicalSize::new(size.width, ui_h));
        }
        if let Some(tab) = self.tabs.borrow().first() {
            // Todas as abas compartilham o mesmo RenderingContext.
            tab.resize(PhysicalSize::new(size.width, size.height.saturating_sub(ui_h).max(1)));
        }
        self.window.request_redraw();
    }

    pub fn rescale(&self) {
        let scale = self.scale();
        if let Some(ui) = self.ui.borrow().as_ref() {
            ui.set_hidpi_scale_factor(scale);
        }
        if let Some(view) = self.popup_view.borrow().as_ref() {
            view.set_hidpi_scale_factor(scale);
        }
        for tab in self.tabs.borrow().iter() {
            tab.set_hidpi_scale_factor(scale);
        }
        self.resize(self.window.inner_size());
    }

    // ------------------------------------------------------------ entrada

    /// Devolve a WebView sob o ponteiro e o ponto local a ela.
    fn target_at(&self, pos: PhysicalPosition<f64>) -> Option<(WebView, DevicePoint, bool)> {
        let ui_h = self.ui_height() as f64;
        if pos.y < ui_h {
            let ui = self.ui.borrow().clone()?;
            Some((ui, DevicePoint::new(pos.x as f32, pos.y as f32), true))
        } else {
            let tab = self.active_tab()?;
            Some((tab, DevicePoint::new(pos.x as f32, (pos.y - ui_h) as f32), false))
        }
    }

    /// Se houver popup aberto sob o ponteiro, devolve a WebView do popup e o ponto local.
    fn popup_target(&self, pos: PhysicalPosition<f64>) -> Option<(WebView, DevicePoint)> {
        let r = self.popup_rect()?;
        if !r.contains(pos.x, pos.y) {
            return None;
        }
        let view = self.popup_view.borrow().clone()?;
        Some((view, DevicePoint::new((pos.x - r.x as f64) as f32, (pos.y - r.y as f64) as f32)))
    }

    pub fn on_cursor_moved(&self, pos: PhysicalPosition<f64>) {
        if let Some((view, point)) = self.popup_target(pos) {
            self.cursor_pos.set(pos);
            view.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point.into())));
            return;
        }
        let previous = self.cursor_pos.replace(pos);
        let ui_h = self.ui_height() as f64;
        // Ao cruzar a fronteira UI/conteúdo, avisa a WebView que o ponteiro saiu.
        if (previous.y < ui_h) != (pos.y < ui_h) {
            if let Some((old, _, _)) = self.target_at(previous) {
                old.notify_input_event(InputEvent::MouseLeftViewport(MouseLeftViewportEvent {
                    focus_moving_to_another_iframe: false,
                }));
            }
        }
        // Durante um arrasto, mantém os eventos na WebView onde o botão foi pressionado.
        let target = match self.pressed_in_ui.get() {
            Some(true) => self
                .ui
                .borrow()
                .clone()
                .map(|ui| (ui, DevicePoint::new(pos.x as f32, pos.y as f32))),
            Some(false) => self
                .active_tab()
                .map(|t| (t, DevicePoint::new(pos.x as f32, (pos.y - ui_h) as f32))),
            None => self.target_at(pos).map(|(w, p, _)| (w, p)),
        };
        if let Some((webview, point)) = target {
            webview.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point.into())));
        }
    }

    pub fn on_cursor_left(&self) {
        for webview in self.ui.borrow().iter().cloned().chain(self.active_tab()) {
            webview.notify_input_event(InputEvent::MouseLeftViewport(MouseLeftViewportEvent {
                focus_moving_to_another_iframe: false,
            }));
        }
    }

    pub fn on_mouse_button(&self, state: ElementState, button: winit::event::MouseButton) {
        let button = match button {
            winit::event::MouseButton::Left => MouseButton::Primary,
            winit::event::MouseButton::Right => MouseButton::Secondary,
            winit::event::MouseButton::Middle => MouseButton::Auxiliary,
            winit::event::MouseButton::Back => {
                if state == ElementState::Pressed {
                    self.handle_ui_message(UiMessage::Back);
                }
                return;
            },
            winit::event::MouseButton::Forward => {
                if state == ElementState::Pressed {
                    self.handle_ui_message(UiMessage::Forward);
                }
                return;
            },
            winit::event::MouseButton::Other(n) => MouseButton::Other(n),
        };
        let pos = self.cursor_pos.get();
        if self.popup.borrow().is_some() {
            if let Some((view, point)) = self.popup_target(pos) {
                let action = match state {
                    ElementState::Pressed => MouseButtonAction::Down,
                    ElementState::Released => MouseButtonAction::Up,
                };
                view.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                    action,
                    button,
                    point.into(),
                )));
            } else if state == ElementState::Pressed {
                // Clique fora fecha o popup e é consumido, como no Chromium.
                self.queue(Command::ClosePopup);
            }
            return;
        }
        let Some((webview, point, in_ui)) = self.target_at(pos) else { return };
        let action = match state {
            ElementState::Pressed => {
                self.pressed_in_ui.set(Some(in_ui));
                if in_ui != self.focus_ui.get() {
                    if in_ui {
                        self.focus_ui.set(true);
                        webview.set_focused(true);
                        if let Some(tab) = self.active_tab() {
                            tab.set_focused(false);
                        }
                    } else {
                        self.focus_content();
                    }
                }
                MouseButtonAction::Down
            },
            ElementState::Released => {
                self.pressed_in_ui.set(None);
                MouseButtonAction::Up
            },
        };
        webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
            action,
            button,
            point.into(),
        )));
    }

    pub fn on_wheel(&self, delta: MouseScrollDelta) {
        let pos = self.cursor_pos.get();
        let popup = self.popup_target(pos);
        if popup.is_none() && self.popup.borrow().is_some() {
            return;
        }
        let Some((webview, point)) = popup.or_else(|| self.target_at(pos).map(|(w, p, _)| (w, p)))
        else {
            return;
        };
        if self.modifiers.get().control_key() {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, dy) => dy as f64,
                MouseScrollDelta::PixelDelta(p) => p.y,
            };
            if dy != 0.0 {
                self.queue(Command::Zoom(if dy > 0.0 { 1 } else { -1 }));
            }
            return;
        }
        let (x, y, mode) = match delta {
            MouseScrollDelta::LineDelta(dx, dy) => {
                ((dx * 76.0) as f64, (dy * 76.0) as f64, WheelMode::DeltaPixel)
            },
            MouseScrollDelta::PixelDelta(d) => (d.x, d.y, WheelMode::DeltaPixel),
        };
        webview.notify_input_event(InputEvent::Wheel(WheelEvent::new(
            WheelDelta { x, y, z: 0.0, mode },
            point.into(),
        )));
    }

    pub fn on_modifiers(&self, modifiers: ModifiersState) {
        self.modifiers.set(modifiers);
    }

    pub fn on_key(&self, event: &KeyEvent) {
        if self.popup.borrow().is_some() {
            if event.state == ElementState::Pressed &&
                event.logical_key == WinitKey::Named(WinitNamedKey::Escape)
            {
                self.queue(Command::ClosePopup);
                return;
            }
            if let Some(view) = self.popup_view.borrow().clone() {
                view.notify_input_event(InputEvent::Keyboard(keyboard_event_from_winit(
                    event,
                    self.modifiers.get(),
                )));
            }
            return;
        }
        if event.state == ElementState::Pressed && self.handle_shortcut(event) {
            return;
        }
        let target = if self.focus_ui.get() {
            self.ui.borrow().clone()
        } else {
            self.active_tab()
        };
        if let Some(webview) = target {
            webview.notify_input_event(InputEvent::Keyboard(keyboard_event_from_winit(
                event,
                self.modifiers.get(),
            )));
        }
    }

    /// Atalhos no estilo Chromium. Retorna `true` se o evento foi consumido.
    fn handle_shortcut(&self, event: &KeyEvent) -> bool {
        let m = self.modifiers.get();
        let ctrl = m.control_key();
        let shift = m.shift_key();
        let alt = m.alt_key();
        let tabs_len = self.tabs.borrow().len();
        let active = self.active.get();

        let command = match (&event.logical_key, ctrl, shift, alt) {
            (WinitKey::Character(c), true, false, false) => match c.to_lowercase().as_str() {
                "t" => Some(Command::NewTab(None)),
                "w" => Some(Command::CloseTab(active)),
                "l" => Some(Command::FocusOmnibox),
                "r" => Some(Command::Ui(UiMessage::Reload)),
                "=" | "+" => Some(Command::Zoom(1)),
                "-" => Some(Command::Zoom(-1)),
                "0" => Some(Command::Zoom(0)),
                "9" => Some(Command::SelectTab(tabs_len.saturating_sub(1))),
                d if d.len() == 1 && ("1"..="8").contains(&d) => {
                    Some(Command::SelectTab(d.parse::<usize>().unwrap() - 1))
                },
                _ => None,
            },
            (WinitKey::Character(c), true, true, false) => match c.to_lowercase().as_str() {
                "+" => Some(Command::Zoom(1)),
                "q" => {
                    self.quit.set(true);
                    return true;
                },
                "n" => {
                    mode::spawn_window(Mode::Private);
                    return true;
                },
                "o" => {
                    mode::spawn_window(Mode::Tor);
                    return true;
                },
                _ => None,
            },
            (WinitKey::Named(WinitNamedKey::Tab), true, false, false) => {
                Some(Command::SelectTab((active + 1) % tabs_len.max(1)))
            },
            (WinitKey::Named(WinitNamedKey::Tab), true, true, false) => {
                Some(Command::SelectTab((active + tabs_len.max(1) - 1) % tabs_len.max(1)))
            },
            (WinitKey::Named(WinitNamedKey::PageDown), true, false, false) => {
                Some(Command::SelectTab((active + 1) % tabs_len.max(1)))
            },
            (WinitKey::Named(WinitNamedKey::PageUp), true, false, false) => {
                Some(Command::SelectTab((active + tabs_len.max(1) - 1) % tabs_len.max(1)))
            },
            (WinitKey::Named(WinitNamedKey::ArrowLeft), false, false, true) => {
                Some(Command::Ui(UiMessage::Back))
            },
            (WinitKey::Named(WinitNamedKey::ArrowRight), false, false, true) => {
                Some(Command::Ui(UiMessage::Forward))
            },
            (WinitKey::Named(WinitNamedKey::Home), false, false, true) => {
                Some(Command::Ui(UiMessage::Home))
            },
            (WinitKey::Named(WinitNamedKey::F5), false, false, false) => {
                Some(Command::Ui(UiMessage::Reload))
            },
            (WinitKey::Named(WinitNamedKey::F6), false, false, false) => {
                Some(Command::FocusOmnibox)
            },
            (WinitKey::Named(WinitNamedKey::F11), false, false, false) => {
                let full = self.window.fullscreen().is_some();
                self.window
                    .set_fullscreen((!full).then_some(Fullscreen::Borderless(None)));
                return true;
            },
            _ => None,
        };
        match command {
            Some(command) => {
                self.queue(command);
                true
            },
            None => false,
        }
    }

    fn queue(&self, command: Command) {
        self.commands.borrow_mut().push(command);
        self.window.request_redraw();
    }
}

/// Converte o favicon do motor em `data:` URL PNG (reduzido para 32 px) para a barra de abas.
fn favicon_data_url(img: &rumen_engine::Image) -> Option<String> {
    use base64::Engine as _;
    use rumen_engine::PixelFormat;

    let (w, h) = (img.width, img.height);
    let px = (w * h) as usize;
    let data = img.data();
    let rgba: Vec<u8> = match img.format {
        PixelFormat::RGBA8 => data.get(..px * 4)?.to_vec(),
        PixelFormat::BGRA8 => data.get(..px * 4)?.chunks(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect(),
        PixelFormat::RGB8 => data.get(..px * 3)?.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        PixelFormat::KA8 => data.get(..px * 2)?.chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        PixelFormat::K8 => data.get(..px)?.iter().flat_map(|&k| [k, k, k, 255]).collect(),
    };
    let mut image = image::RgbaImage::from_raw(w, h, rgba)?;
    if w > 32 || h > 32 {
        image = image::imageops::resize(&image, 32, 32, image::imageops::FilterType::Triangle);
    }
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
}

/// Scripts de várias linhas viram uma função, para que `return` funcione.
fn request_js(js: &str) -> String {
    if js.contains("return ") {
        format!("(function() {{ {js} }})()")
    } else {
        js.to_owned()
    }
}

/// Domínios ENS (`.eth`) são resolvidos pelo gateway eth.limo.
fn web3_gateway(url: Url) -> Url {
    match url.host_str() {
        Some(host) if host.ends_with(".eth") => {
            let mut out = url.clone();
            let _ = out.set_host(Some(&format!("{host}.limo")));
            let _ = out.set_scheme("https");
            out
        },
        _ => url,
    }
}

fn ui_height_px(scale: f64) -> u32 {
    (UI_HEIGHT_CSS as f64 * scale).round() as u32
}

fn cursor_icon(cursor: Cursor) -> Option<CursorIcon> {
    Some(match cursor {
        Cursor::None => return None,
        Cursor::Default => CursorIcon::Default,
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::ContextMenu => CursorIcon::ContextMenu,
        Cursor::Help => CursorIcon::Help,
        Cursor::Progress => CursorIcon::Progress,
        Cursor::Wait => CursorIcon::Wait,
        Cursor::Cell => CursorIcon::Cell,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Text => CursorIcon::Text,
        Cursor::VerticalText => CursorIcon::VerticalText,
        Cursor::Alias => CursorIcon::Alias,
        Cursor::Copy => CursorIcon::Copy,
        Cursor::Move => CursorIcon::Move,
        Cursor::NoDrop => CursorIcon::NoDrop,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::EResize => CursorIcon::EResize,
        Cursor::NResize => CursorIcon::NResize,
        Cursor::NeResize => CursorIcon::NeResize,
        Cursor::NwResize => CursorIcon::NwResize,
        Cursor::SResize => CursorIcon::SResize,
        Cursor::SeResize => CursorIcon::SeResize,
        Cursor::SwResize => CursorIcon::SwResize,
        Cursor::WResize => CursorIcon::WResize,
        Cursor::EwResize => CursorIcon::EwResize,
        Cursor::NsResize => CursorIcon::NsResize,
        Cursor::NeswResize => CursorIcon::NeswResize,
        Cursor::NwseResize => CursorIcon::NwseResize,
        Cursor::ColResize => CursorIcon::ColResize,
        Cursor::RowResize => CursorIcon::RowResize,
        Cursor::AllScroll => CursorIcon::AllScroll,
        Cursor::ZoomIn => CursorIcon::ZoomIn,
        Cursor::ZoomOut => CursorIcon::ZoomOut,
    })
}

impl WebViewDelegate for Browser {
    fn notify_new_frame_ready(&self, _: WebView) {
        self.window.request_redraw();
    }

    fn notify_url_changed(&self, webview: WebView, _url: Url) {
        if !self.is_ui(&webview) {
            self.mark_dirty();
        }
    }

    fn notify_page_title_changed(&self, webview: WebView, _title: Option<String>) {
        if !self.is_ui(&webview) {
            self.mark_dirty();
        }
    }

    fn notify_favicon_changed(&self, webview: WebView) {
        if self.is_ui(&webview) || self.is_popup(&webview) {
            return;
        }
        let encoded = webview.favicon().and_then(|img| favicon_data_url(&img));
        match encoded {
            Some(url) => self.favicons.borrow_mut().insert(webview.id(), url),
            None => self.favicons.borrow_mut().remove(&webview.id()),
        };
        self.mark_dirty();
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        if status == LoadStatus::Started {
            self.favicons.borrow_mut().remove(&webview.id());
        }
        tracing::debug!("load {:?} {:?} {:?}", webview.id(), status, webview.url());
        if !self.is_ui(&webview) {
            self.mark_dirty();
        }
    }

    fn notify_history_changed(&self, webview: WebView, _entries: Vec<Url>, _current: usize) {
        if !self.is_ui(&webview) {
            self.mark_dirty();
        }
    }

    fn notify_cursor_changed(&self, _webview: WebView, cursor: Cursor) {
        match cursor_icon(cursor) {
            Some(icon) => {
                self.window.set_cursor_visible(true);
                self.window.set_cursor(icon);
            },
            None => self.window.set_cursor_visible(false),
        }
    }

    fn notify_fullscreen_state_changed(&self, _webview: WebView, is_fullscreen: bool) {
        self.window
            .set_fullscreen(is_fullscreen.then_some(Fullscreen::Borderless(None)));
    }

    fn notify_closed(&self, webview: WebView) {
        let index = self.tabs.borrow().iter().position(|t| t.id() == webview.id());
        if let Some(index) = index {
            self.queue(Command::CloseTab(index));
        }
    }

    fn notify_crashed(&self, webview: WebView, reason: String, _backtrace: Option<String>) {
        tracing::error!("WebView {:?} travou: {reason}", webview.id());
        self.mark_dirty();
    }

    fn request_create_new(&self, _parent: WebView, request: CreateNewWebViewRequest) {
        // target=_blank / window.open() → nova aba em primeiro plano.
        let webview = request
            .builder(self.content_context.clone())
            .hidpi_scale_factor(self.scale())
            .delegate(self.delegate())
            .build();
        self.adopt_tab(webview);
    }

    fn show_embedder_control(&self, webview: WebView, control: EmbedderControl) {
        let is_active = self.active_tab().is_some_and(|t| t.id() == webview.id());
        if !is_active {
            return; // descartar = cancelar
        }
        let control = match control {
            EmbedderControl::FilePicker(picker) => {
                if self.file_pick.borrow().is_some() {
                    return; // já há um diálogo aberto; descartar = cancelar
                }
                let patterns = picker.filter_patterns().iter().map(|p| p.0.clone()).collect();
                let rx = filepicker::spawn(
                    picker.allow_select_multiple(),
                    patterns,
                    self.waker.clone_box(),
                );
                *self.file_pick.borrow_mut() = Some((picker, rx));
                return;
            },
            other => other,
        };
        if let Some(popup) = Popup::from_control(control, &self.popup_layout()) {
            if let Some(old) = self.popup.borrow_mut().replace(popup) {
                old.cancel();
            }
            self.queue(Command::ShowPopup);
        }
    }

    fn hide_embedder_control(&self, _webview: WebView, control_id: EmbedderControlId) {
        if self.popup.borrow().as_ref().is_some_and(|p| p.id == control_id) {
            self.queue(Command::ClosePopup);
        }
    }

    fn show_console_message(&self, webview: WebView, level: ConsoleLogLevel, message: String) {
        if self.is_ui(&webview) || self.is_popup(&webview) {
            if let Some(json) = message.strip_prefix("RUMEN:") {
                match serde_json::from_str::<UiMessage>(json) {
                    Ok(msg) => self.queue(Command::Ui(msg)),
                    Err(e) => tracing::warn!("mensagem da UI inválida {json}: {e}"),
                }
            }
            return;
        }
        if matches!(level, ConsoleLogLevel::Error | ConsoleLogLevel::Warn) {
            tracing::debug!("console: {message}");
        }
    }
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rumen")
}
