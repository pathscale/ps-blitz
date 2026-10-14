use std::cell::Cell;
use std::rc::Rc;

use web_time::{Duration, Instant};

pub struct JobBudget {
    duration: Cell<Duration>,
    deadline: Cell<Option<Instant>>,
}

impl Default for JobBudget {
    fn default() -> Self {
        Self {
            duration: Cell::new(Duration::from_millis(16)),
            deadline: Cell::new(None),
        }
    }
}

impl JobBudget {
    pub fn duration(&self) -> Duration {
        self.duration.get()
    }

    pub fn set_duration(&self, duration: Duration) {
        assert!(!duration.is_zero(), "the job budget must be positive");
        self.duration.set(duration);
    }

    pub fn begin(self: &Rc<Self>) -> JobSlice {
        let owns_deadline = self.deadline.get().is_none();
        if owns_deadline {
            self.deadline
                .set(Some(Instant::now() + self.duration.get()));
        }
        JobSlice {
            budget: Rc::clone(self),
            owns_deadline,
        }
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.get().map_or(self.duration.get(), |deadline| {
            deadline.saturating_duration_since(Instant::now())
        })
    }

    pub fn exhausted(&self) -> bool {
        self.deadline
            .get()
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
}

/// Nested drains share the owning poll's deadline.
pub struct JobSlice {
    budget: Rc<JobBudget>,
    owns_deadline: bool,
}

impl Drop for JobSlice {
    fn drop(&mut self) {
        if self.owns_deadline {
            self.budget.deadline.set(None);
        }
    }
}
