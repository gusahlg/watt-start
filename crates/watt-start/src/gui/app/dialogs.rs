//! The generic confirmation dialog (delete an instance, maintenance warnings).

use watt_core::ops::OpRequest;

use super::{App, Confirm, ConfirmAction, MainView};
use crate::gui::theme;

impl App {
    pub(crate) fn ask_delete_instance(&mut self, instance: &str) {
        self.confirm = Some(Confirm {
            title: format!("Delete {instance}?"),
            body: "This removes the instance's manifest and metadata. Its store entries and \
                   source snapshots stay until maintenance runs. This cannot be undone."
                .to_owned(),
            type_to_confirm: Some(instance.to_owned()),
            typed: String::new(),
            on_confirm: ConfirmAction::DeleteInstance(instance.to_owned()),
        });
    }

    pub(crate) fn ask_maintenance(&mut self, title: &str, body: &str, request: OpRequest) {
        self.confirm = Some(Confirm {
            title: title.to_owned(),
            body: body.to_owned(),
            type_to_confirm: None,
            typed: String::new(),
            on_confirm: ConfirmAction::Maintenance(request),
        });
    }

    pub(crate) fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &mut self.confirm else {
            return;
        };
        let mut do_confirm = false;
        let mut close = false;
        let ok_enabled = match &confirm.type_to_confirm {
            Some(name) => &confirm.typed == name,
            None => true,
        };
        egui::Modal::new(egui::Id::unique("confirm")).show(ctx, |ui| {
            ui.set_min_width(380.0);
            ui.heading(&confirm.title);
            ui.add_space(8.0);
            ui.label(&confirm.body);
            if let Some(name) = &confirm.type_to_confirm {
                ui.add_space(8.0);
                ui.label(theme::muted(format!("Type \"{name}\" to confirm.")));
                ui.text_edit_singleline(&mut confirm.typed);
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(ok_enabled, egui::Button::new("Confirm"))
                        .clicked()
                    {
                        do_confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        });

        if do_confirm {
            let confirm = self.confirm.take().unwrap();
            match confirm.on_confirm {
                ConfirmAction::DeleteInstance(name) => {
                    self.start_op(
                        name.clone(),
                        OpRequest::Remove {
                            instance: name.clone(),
                        },
                    );
                    if matches!(&self.main_view, Some(MainView::Instance(n)) if n == &name) {
                        self.main_view = self
                            .backend
                            .instances()
                            .iter()
                            .map(|e| e.card.name.clone())
                            .find(|n| n != &name)
                            .map(MainView::Instance);
                    }
                }
                ConfirmAction::Maintenance(request) => {
                    self.start_op("__maintenance__", request);
                }
            }
        } else if close {
            self.confirm = None;
        }
    }
}
