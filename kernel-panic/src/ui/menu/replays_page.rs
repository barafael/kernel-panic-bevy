//! The launch menu's Replays page (M3, `net::replay_log`): the recorded
//! matches, newest first, eight to a page, each with Play (and on the
//! web Save, a download). Natively the list is the `replays/` folder;
//! on the web it is this page session's matches plus the files opened
//! with "Open file…". Play starts the viewer (`ui::hud::replay_bar`).

use std::collections::BTreeMap;

use bevy::prelude::*;

use super::{
    Anchor, BUTTON_GREEN, ButtonSpec, EASY_CYAN, LabelSpec, MenuAction, MenuPage, NAV_BLUE, TEAL,
    VERY_HARD_RED, back_button, page_heading,
};
use crate::net::replay_log::{self, RecentReplays};

/// Replays a page lists.
const PER_PAGE: usize = 8;

/// What the Replays page's buttons ask.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplaysAction {
    /// Play the `n`-th replay of the list.
    Play(u16),
    /// Web: download the `n`-th replay.
    Save(u16),
    /// Step the page.
    Page(i32),
    /// Web: open a `.kpreplay` file.
    Open,
}

/// Where a listed replay's bytes are.
#[derive(Clone, Debug)]
enum Source {
    #[cfg(not(target_arch = "wasm32"))]
    File(std::path::PathBuf),
    /// [`RecentReplays`] entry `n`.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Memory(usize),
}

/// The page's list.
#[derive(Resource, Default)]
pub(crate) struct ReplayShelf {
    /// Every replay known, newest first: its name and where it is.
    entries: Vec<(String, Source)>,
    /// One line per replay seen (by name): its match, or why it can't
    /// be read.
    labels: BTreeMap<String, String>,
    page: usize,
    /// The last thing worth saying (a replay that would not open).
    notice: Option<String>,
}

pub(super) struct ReplaysPagePlugin;

impl Plugin for ReplaysPagePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ReplayShelf>()
            .add_message::<ReplaysAction>()
            .add_systems(
                Update,
                (refresh_shelf, replays_actions)
                    .chain()
                    .before(super::maintain_launch_menu)
                    .run_if(in_state(crate::game_setup::AppState::Menu)),
            );
        #[cfg(target_arch = "wasm32")]
        app.add_systems(Update, web::take_opened_files);
    }
}

/// Re-read the list when the page is entered (natively the folder may
/// have a new file) or the web's list changed.
fn refresh_shelf(page: Res<MenuPage>, recent: Res<RecentReplays>, mut shelf: ResMut<ReplayShelf>) {
    if *page != MenuPage::Replays || !(page.is_changed() || recent.is_changed()) {
        return;
    }
    let shelf = &mut *shelf;
    shelf.entries.clear();
    #[cfg(not(target_arch = "wasm32"))]
    for path in replay_log::list_files(&replay_log::replay_dir()) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        shelf.entries.push((name, Source::File(path)));
    }
    for (i, (name, _)) in recent.0.iter().enumerate() {
        shelf.entries.push((name.clone(), Source::Memory(i)));
    }
    shelf.page = shelf
        .page
        .min(shelf.entries.len().saturating_sub(1) / PER_PAGE);
    // Labels of the shown page (a file is read once).
    let shown: Vec<(String, Source)> = shelf
        .entries
        .iter()
        .skip(shelf.page * PER_PAGE)
        .take(PER_PAGE)
        .cloned()
        .collect();
    for (name, source) in shown {
        if shelf.labels.contains_key(&name) {
            continue;
        }
        let label = match read(&source, &recent).and_then(|b| replay_log::open(&b)) {
            Ok(r) => r.label(),
            Err(why) => format!("{name}: {why}"),
        };
        shelf.labels.insert(name, label);
    }
}

/// The bytes of a listed replay.
fn read(source: &Source, recent: &RecentReplays) -> Result<Vec<u8>, String> {
    match source {
        #[cfg(not(target_arch = "wasm32"))]
        Source::File(path) => std::fs::read(path).map_err(|e| e.to_string()),
        Source::Memory(i) => recent
            .0
            .get(*i)
            .map(|(_, b)| b.clone())
            .ok_or_else(|| "gone".to_string()),
    }
}

fn replays_actions(
    mut actions: MessageReader<ReplaysAction>,
    mut shelf: ResMut<ReplayShelf>,
    recent: Res<RecentReplays>,
    mut page: ResMut<MenuPage>,
    mut commands: Commands,
) {
    for &action in actions.read() {
        let shelf = &mut *shelf;
        match action {
            ReplaysAction::Page(step) => {
                let pages = shelf.entries.len().div_ceil(PER_PAGE).max(1);
                shelf.page = (shelf.page as i32 + step).rem_euclid(pages as i32) as usize;
                // Read the new page's labels.
                page.set_changed();
            }
            ReplaysAction::Play(n) => {
                let Some((name, source)) = shelf.entries.get(n as usize).cloned() else {
                    continue;
                };
                match read(&source, &recent).and_then(|b| replay_log::open(&b)) {
                    Ok(replay) => {
                        shelf.notice = None;
                        replay_log::start(&mut commands, replay, name);
                        *page = MenuPage::Main;
                    }
                    Err(why) => shelf.notice = Some(why),
                }
            }
            ReplaysAction::Save(_n) => {
                #[cfg(target_arch = "wasm32")]
                if let Some((name, source)) = shelf.entries.get(_n as usize)
                    && let Ok(bytes) = read(source, &recent)
                {
                    replay_log::download(name, &bytes);
                }
            }
            ReplaysAction::Open => {
                #[cfg(target_arch = "wasm32")]
                web::open_file();
            }
        }
    }
}

/// The page: a row per replay of the current page, page arrows, Back.
pub(super) fn replays_page(
    commands: &mut Commands,
    root: Entity,
    page_size: f32,
    shelf: &ReplayShelf,
) {
    page_heading(commands, root, page_size, "Replays");
    let size = page_size * 0.7;
    if shelf.entries.is_empty() {
        let where_ = if cfg!(target_arch = "wasm32") {
            "Every match you play here is recorded: it shows up once it ends."
        } else {
            "Every match is recorded into the replays folder: none yet."
        };
        LabelSpec::new(where_, size, (0.5, 0.6), Anchor::Cc).spawn(commands, root);
    }
    let first = shelf.page * PER_PAGE;
    for (i, (name, _)) in shelf.entries.iter().enumerate().skip(first).take(PER_PAGE) {
        let y = 0.78 - 0.065 * (i - first) as f32;
        let label = shelf.labels.get(name).map_or(name.as_str(), String::as_str);
        LabelSpec::new(label, size, (0.08, y), Anchor::Lc).spawn(commands, root);
        ButtonSpec::new(
            "Play",
            BUTTON_GREEN,
            size,
            (0.92, y),
            Anchor::Rc,
            MenuAction::Replays(ReplaysAction::Play(i as u16)),
        )
        .spawn(commands, root);
        if cfg!(target_arch = "wasm32") {
            ButtonSpec::new(
                "Save",
                TEAL,
                size,
                (0.83, y),
                Anchor::Rc,
                MenuAction::Replays(ReplaysAction::Save(i as u16)),
            )
            .spawn(commands, root);
        }
    }
    let pages = shelf.entries.len().div_ceil(PER_PAGE);
    if pages > 1 {
        ButtonSpec::new(
            "<",
            NAV_BLUE,
            size,
            (0.42, 0.22),
            Anchor::Cc,
            MenuAction::Replays(ReplaysAction::Page(-1)),
        )
        .spawn(commands, root);
        LabelSpec::new(
            &format!("{} / {pages}", shelf.page + 1),
            size,
            (0.5, 0.22),
            Anchor::Cc,
        )
        .spawn(commands, root);
        ButtonSpec::new(
            ">",
            NAV_BLUE,
            size,
            (0.58, 0.22),
            Anchor::Cc,
            MenuAction::Replays(ReplaysAction::Page(1)),
        )
        .spawn(commands, root);
    }
    if cfg!(target_arch = "wasm32") {
        ButtonSpec::new(
            "Open file…",
            EASY_CYAN,
            size,
            (0.3, 0.12),
            Anchor::Cc,
            MenuAction::Replays(ReplaysAction::Open),
        )
        .spawn(commands, root);
    }
    if let Some(notice) = &shelf.notice {
        LabelSpec::new(notice, size, (0.5, 0.17), Anchor::Cc)
            .frame(VERY_HARD_RED)
            .spawn(commands, root);
    }
    back_button(page_size, (0.5, 0.08), MenuPage::Main).spawn(commands, root);
}

/// Web: "Open file…" through a hidden `<input type=file>`.
#[cfg(target_arch = "wasm32")]
mod web {
    use std::sync::Mutex;

    use bevy::prelude::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    use crate::net::replay_log::RecentReplays;

    /// Files read by the browser, waiting for the next frame.
    static OPENED: Mutex<Vec<(String, Vec<u8>)>> = Mutex::new(Vec::new());

    /// Ask the browser for a `.kpreplay` file; its bytes land in
    /// [`OPENED`] when read.
    pub fn open_file() {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let Some(input) = document
            .create_element("input")
            .ok()
            .and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        input.set_type("file");
        input.set_accept(".kpreplay");
        let picked = input.clone();
        let on_change = Closure::<dyn FnMut()>::new(move || {
            let Some(file) = picked.files().and_then(|f| f.get(0)) else {
                return;
            };
            let name = file.name();
            let read = wasm_bindgen_futures::JsFuture::from(file.array_buffer());
            wasm_bindgen_futures::spawn_local(async move {
                match read.await {
                    Ok(buffer) => {
                        let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                        if let Ok(mut opened) = OPENED.lock() {
                            opened.push((name, bytes));
                        }
                    }
                    Err(e) => warn!("could not read {name}: {e:?}"),
                }
            });
        });
        input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
        on_change.forget();
        input.click();
    }

    /// Opened files join the list, first.
    pub fn take_opened_files(mut recent: ResMut<RecentReplays>) {
        let opened: Vec<(String, Vec<u8>)> = OPENED
            .lock()
            .map(|mut o| std::mem::take(&mut *o))
            .unwrap_or_default();
        for file in opened {
            recent.0.insert(0, file);
        }
    }
}
