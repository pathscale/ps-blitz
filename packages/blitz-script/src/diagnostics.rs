#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "debug-control"), allow(dead_code))]
pub struct DiagnosticEntry {
    pub sequence: u64,
    pub order: u64,
    pub level: String,
    pub message: String,
    pub stack: String,
}

#[derive(Debug, Clone)]
pub struct DiagnosticStreamBatch {
    pub entries: Vec<DiagnosticEntry>,
    pub lost: u64,
    pub next_sequence: u64,
}

#[derive(Debug, Clone)]
pub struct DiagnosticBatch {
    pub console: DiagnosticStreamBatch,
    pub errors: DiagnosticStreamBatch,
}
