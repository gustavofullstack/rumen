/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Motor do RUMEN.
//!
//! O navegador (`rumen`) só conversa com este crate, nunca com o Servo diretamente.
//! Hoje o motor delega a renderização aos componentes do Servo 0.7 (Stylo, layout,
//! WebRender, rede em hyper/rustls, JS em SpiderMonkey). Assim cada subsistema pode ser
//! substituído por uma implementação própria sem mexer no navegador.

use std::path::PathBuf;

pub use servo::*;

/// Identificação do motor exposta à interface (`rumen --version`, página "sobre").
pub const ENGINE_NAME: &str = "RUMEN Engine";
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const UPSTREAM: &str = "Servo 0.7.0";

/// Configuração do motor para uma janela/sessão.
#[derive(Clone, Debug, Default)]
pub struct EngineConfig {
    /// Onde ficam cookies, localStorage, IndexedDB e cache. Na navegação privada é um
    /// diretório temporário apagado ao sair.
    pub profile_dir: PathBuf,
    /// Proxy HTTP(S) para todo o tráfego, por exemplo o `HTTPTunnelPort` do Tor.
    pub proxy: Option<String>,
    /// Usa um user-agent genérico para reduzir o fingerprint.
    pub hardened: bool,
}

/// Cria a instância do motor com a configuração do RUMEN.
pub fn build(waker: Box<dyn EventLoopWaker>, config: &EngineConfig) -> Servo {
    let _ = std::fs::create_dir_all(&config.profile_dir);
    let opts = Opts {
        config_dir: Some(config.profile_dir.clone()),
        ..Default::default()
    };
    let mut prefs = Preferences::default();
    if let Some(proxy) = &config.proxy {
        prefs.network_http_proxy_uri = proxy.clone();
        prefs.network_https_proxy_uri = proxy.clone();
    }
    if config.hardened {
        // UA genérico e estável: todos os usuários do modo endurecido parecem iguais.
        prefs.user_agent =
            "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0".into();
    }
    let servo = ServoBuilder::default()
        .opts(opts)
        .preferences(prefs)
        .event_loop_waker(waker)
        .build();
    servo.setup_logging();
    servo
}
