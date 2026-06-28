use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
	Copy,
	Move,
}

impl TransferMode {
	pub fn prompt_label(self) -> &'static str {
		match self {
			Self::Copy => "Copy Files",
			Self::Move => "Move Files",
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
	Exact,
	High,
	Ambiguous,
	Unknown,
}

impl Confidence {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Exact => "exact",
			Self::High => "high",
			Self::Ambiguous => "ambiguous",
			Self::Unknown => "unknown",
		}
	}
}

#[derive(Debug, Clone)]
pub struct PlannedMove {
	pub source: PathBuf,
	pub destination: Option<PathBuf>,
	pub platform_slug: Option<String>,
	pub confidence: Confidence,
	pub reason: String,
	pub has_conflict: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Summary {
	pub scanned_files: usize,
	pub planned_moves: usize,
	pub unclassified: usize,
	pub conflicts: usize,
	pub skipped: usize,
}
