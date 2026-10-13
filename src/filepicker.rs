/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Seletor de arquivos nativo (`<input type=file>`) via portal XDG
//! (`org.freedesktop.portal.FileChooser`). No Hyprland/GNOME/KDE abre o diálogo do sistema.
//! Roda numa thread própria para não travar o loop de eventos.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
use rumen_engine::EventLoopWaker;
use url::Url;

/// `None` = cancelado ou portal indisponível.
pub type PickResult = Option<Vec<PathBuf>>;

pub fn spawn(
    multiple: bool,
    patterns: Vec<String>,
    waker: Box<dyn EventLoopWaker>,
) -> Receiver<PickResult> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()
            .and_then(|rt| rt.block_on(pick(multiple, patterns)));
        let _ = tx.send(result);
        waker.wake();
    });
    rx
}

async fn pick(multiple: bool, patterns: Vec<String>) -> PickResult {
    let mut request = SelectedFiles::open_file()
        .title(if multiple { "Escolher arquivos" } else { "Escolher arquivo" })
        .modal(true)
        .multiple(multiple);
    if !patterns.is_empty() {
        let mut filter = FileFilter::new("Arquivos aceitos");
        for p in &patterns {
            // O motor envia extensões ("png") ou tipos MIME ("image/png").
            filter = if p.contains('/') {
                filter.mimetype(p)
            } else {
                filter.glob(&format!("*.{}", p.trim_start_matches('.')))
            };
        }
        request = request.filter(filter);
    }
    let files = match request.send().await.and_then(|r| r.response()) {
        Ok(files) => files,
        Err(e) => {
            tracing::warn!("seletor de arquivos: {e}");
            return None;
        },
    };
    let paths = files
        .uris()
        .iter()
        .filter_map(|u| Url::parse(u.as_str()).ok()?.to_file_path().ok())
        .collect::<Vec<_>>();
    (!paths.is_empty()).then_some(paths)
}
