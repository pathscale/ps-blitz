use std::fmt;
use std::sync::OnceLock;
use taffy::{LayoutInput, LayoutOutput, RunMode};

const MEASUREMENT_LIMIT: usize = 32;

fn measurements_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("BLITZ_GRIDCACHE").as_deref() != Ok("0"))
}

struct Measurement {
    inputs: LayoutInput,
    output: LayoutOutput,
}

impl Measurement {
    fn matches(&self, inputs: &LayoutInput) -> bool {
        self.inputs.known_dimensions == inputs.known_dimensions
            && self.inputs.parent_size == inputs.parent_size
            && self.inputs.available_space == inputs.available_space
            && self.inputs.sizing_mode == inputs.sizing_mode
            && self.inputs.run_mode == inputs.run_mode
            && self.inputs.axis == inputs.axis
            && self.inputs.vertical_margins_are_collapsible
                == inputs.vertical_margins_are_collapsible
    }
}

/// Taffy's cache plus bounded reuse of exact flex/grid measurements.
///
/// Taffy's normal lookup remains first, including its dimension matching and
/// final-layout entry. The additional entries answer only `ComputeSize`.
/// Multiple final-layout entries would require restoring descendant boxes,
/// which a cached `LayoutOutput` alone cannot do.
///
/// Exact measurements include every input, including percentage bases and
/// sizing mode. They survive subsequent measurements and final layout until
/// the owning node is invalidated. Only independent flex/grid containers use
/// this store; block layout can depend on an external float context.
pub struct Cache {
    taffy: taffy::Cache,
    measurements: Vec<Measurement>,
    next_measurement: usize,
}

impl Cache {
    pub const fn new() -> Self {
        Self {
            taffy: taffy::Cache::new(),
            measurements: Vec::new(),
            next_measurement: 0,
        }
    }

    #[inline]
    pub fn get(&self, inputs: &LayoutInput) -> Option<LayoutOutput> {
        let output = self.taffy.get(inputs);
        if output.is_some()
            || inputs.run_mode != RunMode::ComputeSize
            || self.measurements.is_empty()
            || !measurements_enabled()
        {
            return output;
        }

        self.measurements
            .iter()
            .find(|entry| entry.matches(inputs))
            .map(|entry| entry.output)
    }

    #[inline]
    pub fn store(&mut self, inputs: &LayoutInput, output: LayoutOutput) {
        self.taffy.store(inputs, output);
    }

    pub fn store_intrinsic(&mut self, inputs: &LayoutInput, output: LayoutOutput) {
        self.store(inputs, output);
        if inputs.run_mode != RunMode::ComputeSize || !measurements_enabled() {
            return;
        }

        if let Some(entry) = self
            .measurements
            .iter_mut()
            .find(|entry| entry.matches(inputs))
        {
            entry.output = output;
            return;
        }

        let entry = Measurement {
            inputs: *inputs,
            output,
        };
        if self.measurements.len() < MEASUREMENT_LIMIT {
            self.measurements.push(entry);
        } else {
            // Bound memory when a container is measured under changing widths.
            self.measurements[self.next_measurement] = entry;
            self.next_measurement = (self.next_measurement + 1) % MEASUREMENT_LIMIT;
        }
    }

    pub fn clear(&mut self) {
        self.taffy.clear();
        self.measurements.clear();
        self.next_measurement = 0;
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Cache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cache")
            .field("taffy", &self.taffy)
            .field("measurements", &self.measurements.len())
            .finish()
    }
}
