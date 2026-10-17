//! The left sidebar: logo, search, grouped instance list, "+ New instance" and "Settings".

use watt_core::meta::Binding;
use watt_core::status::Readiness;

use super::{App, MainView};
use crate::gui::backend::InstanceEntry;
use crate::gui::theme;

impl App {
    pub(crate) fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            (self.forge.cube_icon)(ui, 26.0, theme::WATT, false);
            ui.label(theme::title("watt-start"));
        });
        ui.add_space(10.0);
        let search_id = ui.make_persistent_id("sidebar_search");
        if self.focus_search {
            self.focus_search = false;
            ui.memory_mut(|mem| mem.request_focus(search_id));
        }
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .id(search_id)
                .hint_text("Search instances")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(8.0);
        ui.separator();

        let needle = self.search.to_lowercase();
        let entries: Vec<InstanceEntry> = self
            .backend
            .instances()
            .iter()
            .filter(|e| {
                needle.is_empty()
                    || e.card
                        .meta
                        .title(&e.card.name)
                        .to_lowercase()
                        .contains(&needle)
                    || e.card.name.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let favorites: Vec<&InstanceEntry> =
                    entries.iter().filter(|e| e.card.meta.favorite).collect();
                if !favorites.is_empty() {
                    self.group_heading(ui, "Favourites");
                    for e in &favorites {
                        self.instance_row(ui, e);
                    }
                }

                let mut groups: Vec<&str> = entries
                    .iter()
                    .filter(|e| !e.card.meta.favorite)
                    .filter_map(|e| e.card.meta.group.as_deref())
                    .collect();
                groups.sort_unstable();
                groups.dedup();
                for group in groups {
                    self.group_heading(ui, group);
                    for e in entries.iter().filter(|e| {
                        !e.card.meta.favorite && e.card.meta.group.as_deref() == Some(group)
                    }) {
                        self.instance_row(ui, e);
                    }
                }

                let rest: Vec<&InstanceEntry> = entries
                    .iter()
                    .filter(|e| !e.card.meta.favorite && e.card.meta.group.is_none())
                    .collect();
                if !rest.is_empty() {
                    if !favorites.is_empty() || !groups_is_empty(&entries) {
                        self.group_heading(ui, "Other");
                    }
                    for e in &rest {
                        self.instance_row(ui, e);
                    }
                }
            });

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);
        if ui.button("+ New instance").clicked() {
            self.open_new_instance_dialog();
        }
        if ui.button("Settings").clicked() {
            self.main_view = Some(MainView::Settings);
        }
        ui.add_space(6.0);
    }

    fn group_heading(&self, ui: &mut egui::Ui, text: &str) {
        ui.add_space(6.0);
        ui.label(theme::muted(text.to_uppercase()));
    }

    fn instance_row(&mut self, ui: &mut egui::Ui, entry: &InstanceEntry) {
        let name = &entry.card.name;
        let selected = matches!(&self.main_view, Some(MainView::Instance(n)) if n == name);
        let accent = theme::accent_for_meta(name, entry.card.meta.color.as_deref());
        let busy = self.op_kind_running(name).is_some() || entry.status.busy;

        let frame = egui::Frame::new()
            .corner_radius(8u8)
            .inner_margin(egui::Margin::symmetric(8, 7))
            .fill(if selected {
                theme::BG_3
            } else {
                egui::Color32::TRANSPARENT
            });
        let response = frame
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    (self.forge.cube_icon)(ui, 20.0, accent, busy);
                    ui.vertical(|ui| {
                        ui.label(entry.card.meta.title(name));
                        ui.label(theme::muted(subtitle(entry)));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        status_badge(ui, entry, self.updates.get(name).map(Vec::len).unwrap_or(0));
                    });
                });
            })
            .response
            .interact(egui::Sense::click());
        if response.clicked() {
            self.main_view = Some(MainView::Instance(name.clone()));
            self.instance_tab = super::InstanceTab::Overview;
        }
    }
}

fn groups_is_empty(entries: &[InstanceEntry]) -> bool {
    !entries
        .iter()
        .any(|e| !e.card.meta.favorite && e.card.meta.group.is_some())
}

fn subtitle(entry: &InstanceEntry) -> String {
    match &entry.card.meta.source {
        Binding::Managed { channel, .. } => format!(
            "{} · {channel}",
            entry.card.game.version.as_deref().unwrap_or("-")
        ),
        Binding::Linked { .. } => {
            format!(
                "linked · {}",
                entry.card.game.branch.as_deref().unwrap_or("?")
            )
        }
        Binding::PwcDefault => "pwc".to_owned(),
    }
}

/// One consistent status element on the row's right: a running dot, an update pill, or a plain
/// readiness dot — never more than one, so nothing pushes anything else out of place.
fn status_badge(ui: &mut egui::Ui, entry: &InstanceEntry, updates: usize) {
    if entry.status.running.is_some() {
        theme::dot(ui, theme::CYAN);
    } else if updates > 0 {
        let label = if updates == 1 {
            "1 update".to_owned()
        } else {
            format!("{updates} updates")
        };
        // A small, quiet pill: a cyan outline rather than a bright fill, so it reads as a status
        // next to the readiness dots rather than shouting over the row.
        egui::Frame::new()
            .fill(theme::BG_2)
            .stroke(egui::Stroke::new(1.0, theme::CYAN))
            .corner_radius(255u8)
            .inner_margin(egui::Margin::symmetric(6, 1))
            .show(ui, |ui| {
                ui.label(egui::RichText::new(label).color(theme::CYAN).size(11.0));
            });
    } else {
        let color = match &entry.status.readiness {
            Readiness::Ready { .. } => theme::GOOD,
            Readiness::New | Readiness::NeedsBuild { .. } => theme::WARN,
            Readiness::Broken { .. } => theme::BAD,
        };
        theme::dot(ui, color);
    }
}
