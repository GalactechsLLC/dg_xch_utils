use std::sync::Arc;
use eframe::egui;
use eframe::egui::{Context};
use crate::app::DgXchGui;
use crate::scenes::Scene;
use crate::state::WalletState;
use egui_plot::{Line, Plot, PlotPoints, BarChart, Bar, Legend};
pub struct WalletOverviewScene {
    wallet_state: Arc<WalletState>,
}

impl WalletOverviewScene {
    pub fn new(wallet_state: Arc<WalletState>) -> Self {
        WalletOverviewScene { wallet_state }
    }
}

impl Scene for WalletOverviewScene {
    fn update(&mut self, gui: &mut DgXchGui, ctx: &Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Wallet Overview");
            ui.separator();
            
            let (confirmed_balance, unconfirmed_balance) = if let Some(ref wal) = gui.wallet {
                (
                    wal.confirmed_balance,
                    wal.unconfirmed_balance,
                )
            } else {
                (0, 0)
            };
    
            ui.label(format!(
                "Confirmed Balance: {:.12} XCH",
                confirmed_balance as f64 / 1_000_000_000_000.0
            ));

            ui.label(format!(
                "Unconfirmed Balance: {:.12} XCH",
                unconfirmed_balance as f64 / 1_000_000_000_000.0
            ));

            ui.add_space(20.0);

            let balance_history = self.wallet_state.balance_history.clone();

            ui.add_space(20.0);

            if balance_history.len() >= 2 {
                let line_points: PlotPoints = balance_history.iter().map(|(block_height, balance)| {
                    [*block_height as f64, *balance as f64 / 1_000_000_000_000.0]
                }).collect();

                let line = Line::new(line_points).name("Total Balance");

                ui.heading("Balance Over Time");

                Plot::new("Balance Over Time")
                    .legend(Legend::default())
                    .show(ui, |plot_ui| {
                        plot_ui.line(line);
                    });

                let gain_loss_bars: Vec<Bar> = balance_history
                    .into_iter().map(|window| {
                        let (_prev_height, prev_balance) = window;
                        let (curr_height, curr_balance) = window;
                        let delta = curr_balance - prev_balance;
                        Bar::new(
                            curr_height as f64,
                            delta as f64 / 1_000_000_000_000.0,
                        )
                    })
                    .collect();

                let bar_chart = BarChart::new(gain_loss_bars).name("Gains/Losses");

                ui.heading("Gains and Losses Over Time");

                Plot::new("Gains and Losses Over Time")
                    .legend(Legend::default())
                    .show(ui, |plot_ui| {
                        plot_ui.bar_chart(bar_chart);
                    });
            } else {
                ui.label("Balance history data is insufficient to display the chart.");
            }
        });

        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
}
