use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::node) enum IndexAction {
    Build,
    Shed,
}

#[derive(Default)]
struct State {
    active: bool,
    built: bool,
    shed: bool,
    retry_at: Option<Instant>,
}

#[derive(Default)]
pub(in crate::node) struct IndexMaintenance {
    state: Mutex<State>,
}

pub(in crate::node) struct IndexJob {
    pub action: IndexAction,
    controller: Arc<IndexMaintenance>,
    finished: bool,
}

impl IndexMaintenance {
    pub fn schedule(self: &Arc<Self>, at_tip: bool, deep_behind: bool) -> Option<IndexJob> {
        self.schedule_at(at_tip, deep_behind, Instant::now())
    }

    fn schedule_at(
        self: &Arc<Self>,
        at_tip: bool,
        deep_behind: bool,
        now: Instant,
    ) -> Option<IndexJob> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.active || state.retry_at.is_some_and(|retry| now < retry) {
            return None;
        }
        let action = if at_tip && !state.built {
            IndexAction::Build
        } else if !at_tip && deep_behind && !state.shed {
            IndexAction::Shed
        } else {
            return None;
        };
        state.active = true;
        Some(IndexJob {
            action,
            controller: self.clone(),
            finished: false,
        })
    }
}

impl IndexJob {
    pub fn finish(mut self, success: bool) {
        let mut state = self
            .controller
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = false;
        if success {
            state.built = self.action == IndexAction::Build;
            state.shed = self.action == IndexAction::Shed;
            state.retry_at = None;
        } else {
            state.retry_at = Some(Instant::now() + Duration::from_secs(30));
        }
        self.finished = true;
    }
}

impl Drop for IndexJob {
    fn drop(&mut self) {
        if !self.finished {
            let mut state = self
                .controller
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.active = false;
            state.retry_at = Some(Instant::now() + Duration::from_secs(30));
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/node/maintenance.rs"]
mod tests;
