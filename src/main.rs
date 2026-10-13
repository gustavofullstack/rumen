/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! RUMEN — navegador no estilo Chromium escrito em Rust, usando o motor Servo.

mod browser;
mod control;
mod filepicker;
mod keyutils;
mod mcp;
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

/// App-id da janela; `RUMEN_APP_ID` permite instâncias de teste isoladas (ex.: "rumen-test").
fn app_id() -> String {
    std::env::var("RUMEN_APP_ID").unwrap_or_else(|_| APP_ID.to_owned())
}

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

    match std::env::args().nth(1).as_deref() {
        // Servidor MCP (stdio) para agentes controlarem o RUMEN aberto.
        Some("mcp") => {
            mcp::run();
            return Ok(());
        },
        // `rumen ctl '{"cmd":"tabs"}'`: um comando direto no socket de controle.
        Some("ctl") => {
            let raw = std::env::args().nth(2).unwrap_or_else(|| r#"{"cmd":"ping"}"#.into());
            let cmd: serde_json::Value = serde_json::from_str(&raw)?;
            println!("{}", serde_json::to_string_pretty(&control::send(&cmd)?)?);
            return Ok(());
        },
        _ => {},
    }

    if std::env::var_os("RUMEN_CHILD").is_none() && !std::env::args().any(|a| a == "--no-supervisor") {
        return supervise();
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

/// Supervisor: roda o navegador como processo filho e, se ele travar (pânico, sinal do
/// sistema), reabre com as abas salvas. Uma saída normal (fechar a janela, Ctrl+Shift+Q)
/// encerra de verdade. Proteção contra laço: no máximo 5 reaberturas em 2 minutos.
fn supervise() -> Result<(), Box<dyn Error>> {
    use std::process::Command;
    use std::time::{Duration, Instant};

    let exe = std::env::current_exe()?;
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let child_pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    {
        // Encaminha SIGINT/SIGTERM ao filho, que salva a sessão e sai limpo.
        let child_pid = child_pid.clone();
        let _ = ctrlc::set_handler(move || {
            TERMINATE.store(true, Ordering::SeqCst);
            let pid = child_pid.load(Ordering::SeqCst);
            if pid != 0 {
                let _ = Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
            }
        });
    }
    let mut restarts: Vec<Instant> = Vec::new();
    loop {
        let mut child = Command::new(&exe).args(&args).env("RUMEN_CHILD", "1").spawn()?;
        child_pid.store(child.id(), Ordering::SeqCst);
        let status = child.wait()?;
        child_pid.store(0, Ordering::SeqCst);
        if status.success() || TERMINATE.load(Ordering::SeqCst) {
            return Ok(());
        }
        restarts.retain(|t| t.elapsed() < Duration::from_secs(120));
        if restarts.len() >= 5 {
            eprintln!("RUMEN: travou 5 vezes em 2 minutos; não vou reabrir automaticamente.");
            std::process::exit(1);
        }
        restarts.push(Instant::now());
        eprintln!("RUMEN: o navegador fechou inesperadamente ({status}); reabrindo com suas abas…");
        // Reabre restaurando a sessão salva (sem as URLs da linha de comando).
        args.retain(|a| a.starts_with('-'));
        std::thread::sleep(Duration::from_millis(800));
    }
}

/// No Wayland, com o intervalo de troca padrão (1), o Mesa bloqueia `eglSwapBuffers` até o
/// compositor mandar o "frame callback", que nunca chega enquanto a janela está numa mesa
/// escondida. Isso congelava o navegador inteiro (inclusive o socket de controle e o
/// encerramento limpo). Com intervalo 0 a troca não bloqueia; o ritmo de pintura continua
/// sendo dado pelos pedidos de redesenho do motor.
fn disable_vsync_block() {
    use std::ffi::c_void;
    #[link(name = "EGL")]
    unsafe extern "C" {
        fn eglGetCurrentDisplay() -> *mut c_void;
        fn eglSwapInterval(display: *mut c_void, interval: i32) -> u32;
    }
    // SAFETY: chamadas EGL sem argumentos de ponteiro do Rust; o display é o atual do contexto
    // que acabou de ser tornado corrente nesta thread.
    unsafe {
        let display = eglGetCurrentDisplay();
        if !display.is_null() && eglSwapInterval(display, 0) == 0 {
            warn!("eglSwapInterval(0) falhou; janela escondida pode congelar a pintura");
        }
    }
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
        let id = app_id();
        let attributes = WindowAttributesExtWayland::with_name(attributes, &id, &id);
        let attributes = WindowAttributesExtX11::with_name(attributes, &id, &id);
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
        disable_vsync_block();

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
            Browser::new(window, servo, rendering_context, resources, urls, session.mode, waker.clone_box());
        // Só a janela normal aceita controle externo (privada/Tor ficam fechadas a isso).
        if session.mode.persistent() {
            if let Some(rx) = control::listen(waker.clone_box()) {
                browser.attach_control(rx);
            }
        }
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
            WindowEvent::Occluded(hidden) => browser.set_occluded(hidden),
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
            browser.save_session();
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
