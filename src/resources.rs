/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Páginas internas (interface e nova aba), embutidas no binário e gravadas em
//! `~/.local/share/rumen/` na inicialização para serem carregadas via `file://`.

use std::fs;
use std::path::{Path, PathBuf};

use url::Url;

const UI_HTML: &str = include_str!("../res/ui.html");
const NEWTAB_HTML: &str = include_str!("../res/newtab.html");
const POPUP_HTML: &str = include_str!("../res/popup.html");

pub struct Resources {
    ui: Url,
    newtab: Url,
    popup: Url,
    root: PathBuf,
}

impl Resources {
    pub fn install(dir: &Path) -> std::io::Result<Self> {
        let root = dir.join("internal");
        fs::create_dir_all(&root)?;
        let ui = root.join("ui.html");
        let newtab = root.join("newtab.html");
        fs::write(&ui, UI_HTML)?;
        fs::write(&newtab, NEWTAB_HTML)?;
        let popup = root.join("popup.html");
        fs::write(&popup, POPUP_HTML)?;
        Ok(Self {
            ui: Url::from_file_path(&ui).expect("caminho absoluto"),
            newtab: Url::from_file_path(&newtab).expect("caminho absoluto"),
            popup: Url::from_file_path(&popup).expect("caminho absoluto"),
            root,
        })
    }

    pub fn ui_url(&self) -> Url {
        self.ui.clone()
    }

    /// Aplica páginas internas de um diretório (atualização ao vivo da interface).
    pub fn apply_dir(&self, dir: &Path) -> std::io::Result<Vec<String>> {
        let mut applied = Vec::new();
        for name in ["ui.html", "newtab.html", "popup.html"] {
            let src = dir.join(name);
            if src.is_file() {
                fs::copy(&src, self.root.join(name))?;
                applied.push(name.to_owned());
            }
        }
        Ok(applied)
    }

    pub fn popup_url(&self) -> Url {
        self.popup.clone()
    }

    pub fn newtab_url(&self) -> Url {
        self.newtab.clone()
    }

    pub fn is_newtab(&self, url: &Url) -> bool {
        url.as_str() == self.newtab.as_str()
    }

    pub fn is_internal(&self, url: &Url) -> bool {
        url.scheme() == "file" &&
            url.to_file_path()
                .is_ok_and(|p| p.starts_with(&self.root))
    }
}
