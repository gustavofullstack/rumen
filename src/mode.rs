/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Modos de navegação: normal (dados salvos), privado (nada fica no disco) e Tor
//! (privado e todo o tráfego passando pela rede Tor, com acesso a sites `.onion`).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use rumen_engine::EngineConfig;

use crate::browser::data_dir;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Private,
    Tor,
}

/// Porta do `HTTPTunnelPort` do Tor iniciado pelo RUMEN (diferente da 9050 do sistema).
const TOR_HTTP_PORT: u16 = 9481;

impl Mode {
    pub fn from_args() -> Self {
        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|a| a == "--tor") {
            Mode::Tor
        } else if args.iter().any(|a| a == "--private" || a == "--incognito") {
            Mode::Private
        } else {
            Mode::Normal
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Private => "private",
            Mode::Tor => "tor",
        }
    }

    pub fn persistent(self) -> bool {
        self == Mode::Normal
    }

    pub fn title_suffix(self) -> &'static str {
        match self {
            Mode::Normal => "RUMEN",
            Mode::Private => "RUMEN (Privado)",
            Mode::Tor => "RUMEN (Tor)",
        }
    }
}

/// Recursos da sessão que precisam ser limpos ao sair (perfil temporário, processo do Tor).
pub struct Session {
    pub mode: Mode,
    pub config: EngineConfig,
    temp_dir: Option<PathBuf>,
    tor: Option<Child>,
}

impl Session {
    pub fn start(mode: Mode) -> Self {
        if mode.persistent() {
            return Self {
                mode,
                config: EngineConfig {
                    profile_dir: data_dir().join("profile"),
                    proxy: None,
                    hardened: false,
                },
                temp_dir: None,
                tor: None,
            };
        }
        purge_stale_sessions();
        let temp = ephemeral_root().join(format!("rumen-{}-{}", mode.label(), std::process::id()));
        let _ = std::fs::create_dir_all(&temp);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o700));
        }
        let mut tor = None;
        let mut proxy = None;
        if mode == Mode::Tor {
            match spawn_tor(&temp.join("tor")) {
                Ok(child) => {
                    tor = Some(child);
                    proxy = Some(format!("http://127.0.0.1:{TOR_HTTP_PORT}"));
                },
                Err(e) => {
                    // Sem Tor não há anonimato: o RUMEN recusa navegar em vez de vazar o IP.
                    eprintln!("RUMEN: não foi possível iniciar o Tor ({e}). Instale com: sudo pacman -S tor");
                    proxy = Some("http://127.0.0.1:9".to_owned());
                },
            }
        }
        Self {
            mode,
            config: EngineConfig {
                profile_dir: temp.join("profile"),
                proxy,
                hardened: true,
            },
            temp_dir: Some(temp),
            tor,
        }
    }

    pub fn tor_running(&self) -> bool {
        self.tor.is_some()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(mut tor) = self.tor.take() {
            let _ = tor.kill();
            let _ = tor.wait();
        }
        if let Some(dir) = self.temp_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Perfis efêmeros ficam em `$XDG_RUNTIME_DIR` (tmpfs em RAM, acessível só pelo usuário).
fn ephemeral_root() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// Apaga perfis privados/Tor de processos que já morreram (ex.: encerrados com kill -9).
fn purge_stale_sessions() {
    for root in [ephemeral_root(), std::env::temp_dir()] {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let pid = name
                .strip_prefix("rumen-private-")
                .or_else(|| name.strip_prefix("rumen-tor-"))
                .and_then(|p| p.parse::<u32>().ok());
            if let Some(pid) = pid {
                if !PathBuf::from(format!("/proc/{pid}")).exists() {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }
}

fn spawn_tor(data_dir: &PathBuf) -> std::io::Result<Child> {
    std::fs::create_dir_all(data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Command::new("tor")
        .args(["--SocksPort", "0", "--HTTPTunnelPort"])
        .arg(format!("127.0.0.1:{TOR_HTTP_PORT}"))
        .arg("--DataDirectory")
        .arg(data_dir)
        .args(["--Log", "notice stderr"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Abre uma nova janela do RUMEN (processo separado) no modo pedido.
pub fn spawn_window(mode: Mode) {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = Command::new(exe);
    match mode {
        Mode::Private => {
            cmd.arg("--private");
        },
        Mode::Tor => {
            cmd.arg("--tor");
        },
        Mode::Normal => {},
    }
    let _ = cmd.stdin(Stdio::null()).spawn();
}
