/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! RUMEN — navegador no estilo Chromium escrito em Rust, usando o motor Servo.

mod browser;
mod keyutils;
mod mode;
mod popup;
mod resources;

use std::error::Error;
use std::fs;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use rumen_engine::{EventLoopWaker, RenderingContext, WindowRenderingContext};
use tracing::warn;
use url::Url;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::platform::x11::WindowAttributesExtX11;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{Window, WindowId};

use crate::browser::{Browser, data_dir};
use crate::mode::{Mode, Session};
use crate::resources::Resources;

const APP_ID: &str = "rumen";

static TERMINATE: AtomicBool = AtomicBool::new(false);

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!(
            "RUMEN {} — {} {} (base: {})",
            env!("CARGO_PKG_VERSION"),
            rumen_engine::ENGINE_NAME,
            rumen_engine::ENGINE_VERSION,
            rumen_engine::UPSTREAM
        );
        return Ok(());
    }

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("falha ao instalar provedor de criptografia");

    let event_loop = EventLoop::with_user_event().build()?;
    // SIGINT/SIGTERM/SIGHUP: sai pelo caminho normal para apagar perfis temporários e parar o Tor.
    let proxy = event_loop.create_proxy();
    let _ = ctrlc::set_handler(move || {
        TERMINATE.store(true, Ordering::SeqCst);
        let _ = proxy.send_event(WakerEvent);
    });
    let mut app = App::Initial(Waker(event_loop.create_proxy()));
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// URLs passadas na linha de comando; sem argumentos, restaura a sessão anterior.
fn initial_urls(resources: &Resources) -> Vec<Url> {
    let args: Vec<Url> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .filter_map(|a| {
            Url::parse(&a)
                .ok()
                .or_else(|| std::path::absolute(&a).ok().and_then(|p| Url::from_file_path(p).ok()))
        })
        .collect();
    if !args.is_empty() {
        return args;
    }
    fs::read_to_string(data_dir().join("session.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|s| Url::parse(s).ok())
        .filter(|u| !resources.is_internal(u))
        .collect()
}

fn save_session(browser: &Browser) {
    let urls = browser.session_urls();
    let path = data_dir().join("session.json");
    if let Err(e) = fs::write(&path, serde_json::to_string_pretty(&urls).unwrap_or_default()) {
        warn!("não foi possível salvar a sessão em {path:?}: {e}");
    }
}

enum App {
    Initial(Waker),
    // A ordem importa: o navegador é destruído antes da sessão (perfil temporário, Tor).
    Running(Rc<Browser>, Session),
    Done,
}

impl ApplicationHandler<WakerEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let Self::Initial(waker) = self else { return };

        let attributes = Window::default_attributes()
            .with_title("RUMEN")
            .with_inner_size(LogicalSize::new(1280.0, 800.0))
            .with_min_inner_size(LogicalSize::new(400.0, 200.0));
        // Define o app-id (Wayland) / WM_CLASS (X11) para regras do Hyprland.
        let attributes = WindowAttributesExtWayland::with_name(attributes, APP_ID, APP_ID);
        let attributes = WindowAttributesExtX11::with_name(attributes, APP_ID, APP_ID);
        let window = event_loop
            .create_window(attributes)
            .expect("falha ao criar janela");

        let display_handle = event_loop.display_handle().expect("display handle");
        let window_handle = window.window_handle().expect("window handle");
        let rendering_context = Rc::new(
            WindowRenderingContext::new(display_handle, window_handle, window.inner_size())
                .expect("falha ao criar contexto OpenGL"),
        );
        let _ = rendering_context.make_current();

        let session = Session::start(Mode::from_args());
        let servo = rumen_engine::build(Box::new(waker.clone()), &session.config);

        let dir = data_dir();
        let resources = Resources::install(&dir).expect("falha ao gravar páginas internas");
        let mut urls = initial_urls(&resources);
        if !session.mode.persistent() {
            // Janela privada/Tor nunca restaura nem grava a sessão.
            urls.retain(|_| std::env::args().skip(1).any(|a| !a.starts_with('-')));
        }
        if session.mode == Mode::Tor && !session.tor_running() {
            urls = vec![resources.newtab_url()];
        }
        let browser =
            Browser::new(window, servo, rendering_context, resources, urls, session.mode);
        *self = Self::Running(browser, session);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: WakerEvent) {
        if TERMINATE.load(Ordering::SeqCst) {
            self.shutdown(event_loop);
            return;
        }
        if let Self::Running(browser, _) = self {
            browser.servo.spin_event_loop();
            browser.flush();
            if browser.should_quit() {
                self.shutdown(event_loop);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Self::Running(browser, _) = self else { return };
        browser.servo.spin_event_loop();

        match event {
            WindowEvent::CloseRequested => {
                self.shutdown(event_loop);
                return;
            },
            WindowEvent::RedrawRequested => browser.paint(),
            WindowEvent::Resized(size) => browser.resize(size),
            WindowEvent::ScaleFactorChanged { .. } => browser.rescale(),
            WindowEvent::CursorMoved { position, .. } => browser.on_cursor_moved(position),
            WindowEvent::CursorLeft { .. } => browser.on_cursor_left(),
            WindowEvent::MouseInput { state, button, .. } => browser.on_mouse_button(state, button),
            WindowEvent::MouseWheel { delta, .. } => browser.on_wheel(delta),
            WindowEvent::ModifiersChanged(modifiers) => browser.on_modifiers(modifiers.state()),
            WindowEvent::KeyboardInput { event, .. } => browser.on_key(&event),
            _ => {},
        }

        browser.flush();
        if browser.should_quit() {
            self.shutdown(event_loop);
        }
    }
}

impl App {
    fn shutdown(&mut self, event_loop: &ActiveEventLoop) {
        if let Self::Running(browser, session) = std::mem::replace(self, Self::Done) {
            if session.mode.persistent() {
                save_session(&browser);
            }
            drop(browser);
            drop(session);
        }
        event_loop.exit();
    }
}

#[derive(Clone)]
struct Waker(EventLoopProxy<WakerEvent>);

#[derive(Debug)]
struct WakerEvent;

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        if let Err(error) = self.0.send_event(WakerEvent) {
            warn!(?error, "falha ao acordar o loop de eventos");
        }
    }
}
