use crate::app::DgXchGui;
use crate::scenes::Scene;
use crate::state::SelectedTab;
use eframe::egui;

pub struct TabBar {
    pub tabs: [(String, SelectedTab); 4],
}
impl TabBar {
    pub fn new(_gui: &DgXchGui) -> Self {
        TabBar {
            tabs: [
                (String::from("Full Node"), SelectedTab::FullNode),
                (String::from("Wallet"), SelectedTab::Wallet),
                (String::from("Farmer"), SelectedTab::Farmer),
                (String::from("Config"), SelectedTab::Config),
            ],
        }
    }
}

impl Scene for TabBar {
    fn update(&mut self, gui: &mut DgXchGui, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("Tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (label, tab) in &self.tabs {
                    if ui
                        .selectable_label(gui.state.selected_tab == *tab, label)
                        .clicked()
                    {
                        gui.state.selected_tab = *tab;
                    }
                }
                ui.separator();
            });
        });
    }
}
