use std::{
    collections::HashSet,
    fs::File,
    path::Path,
};

use anyhow::{Context, Result};
use serde::Deserialize;
use zip::ZipArchive;

use crate::model::Confidence;

#[derive(Debug, Clone, Deserialize)]
pub struct PlatformRule {
    pub slug: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub folder_hints: Vec<String>,
    #[serde(default)]
    pub dat_tokens: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PlatformIndex {
    platforms: Vec<PlatformRule>,
}

#[derive(Debug, Clone)]
pub struct Classification {
    pub platform_slug: Option<String>,
    pub confidence: Confidence,
    pub reason: String,
}

pub struct Classifier {
    rules: Vec<PlatformRule>,
}

impl Classifier {
    pub fn from_embedded() -> Result<Self> {
        let raw = include_str!("../assets/platform_index.json");
        let parsed: PlatformIndex =
            serde_json::from_str(raw).context("Failed to parse embedded platform index")?;
        Ok(Self {
            rules: parsed.platforms,
        })
    }

    pub fn classify(&self, path: &Path) -> Classification {
        let extension = path
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        let zip_member_extensions = if extension == "zip" {
            inspect_zip_member_extensions(path, 200)
        } else {
            None
        };

        let file_name = path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        let folders: Vec<String> = path
            .ancestors()
            .take(6)
            .filter_map(|p| p.file_name().and_then(|s| s.to_str()))
            .map(|s| s.to_ascii_lowercase())
            .collect();

        let bracket_tokens = extract_bracket_tokens(&file_name);

        let mut best_slug: Option<&str> = None;
        let mut best_score = 0u32;
        let mut best_reason = String::from("no classifier signal matched");
        let mut tied = false;

        for rule in &self.rules {
            let mut score = 0u32;
            let mut reasons = Vec::new();

            if !extension.is_empty()
                && rule
                    .extensions
                    .iter()
                    .any(|ext| ext.eq_ignore_ascii_case(&extension))
            {
                score += 70;
                reasons.push(format!("extension .{extension}"));
            }

            if rule
                .folder_hints
                .iter()
                .any(|hint| folders.iter().any(|f| f.contains(&hint.to_ascii_lowercase())))
            {
                score += 40;
                reasons.push(String::from("folder hint"));
            }

            if rule
                .aliases
                .iter()
                .any(|alias| file_name.contains(&alias.to_ascii_lowercase()))
            {
                score += 30;
                reasons.push(String::from("filename alias"));
            }

            if rule.dat_tokens.iter().any(|token| {
                bracket_tokens
                    .iter()
                    .any(|t| t.contains(&token.to_ascii_lowercase()))
            }) {
                score += 50;
                reasons.push(String::from("dat token"));
            }

            if let Some(member_exts) = &zip_member_extensions {
                if let Some(found_ext) = member_exts.iter().find(|member_ext| {
                    rule.extensions
                        .iter()
                        .any(|ext| ext.eq_ignore_ascii_case(member_ext))
                }) {
                    score += 90;
                    reasons.push(format!("zip member extension .{found_ext}"));
                }
            }

            if score == 0 {
                continue;
            }

            if score > best_score {
                best_score = score;
                best_slug = Some(rule.slug.as_str());
                tied = false;
                best_reason = reasons.join(", ");
            } else if score == best_score {
                tied = true;
            }
        }

        if let Some(slug) = best_slug {
            let confidence = if tied {
                Confidence::Ambiguous
            } else if best_score >= 100 {
                Confidence::Exact
            } else {
                Confidence::High
            };

            return Classification {
                platform_slug: Some(slug.to_owned()),
                confidence,
                reason: if tied {
                    String::from("multiple platforms matched with equal score")
                } else {
                    best_reason
                },
            };
        }

        Classification {
            platform_slug: None,
            confidence: Confidence::Unknown,
            reason: String::from("no platform match"),
        }
    }

    pub fn allowed_extensions(&self) -> HashSet<String> {
        self.rules
            .iter()
            .flat_map(|rule| rule.extensions.iter())
            .map(|ext| ext.to_ascii_lowercase())
            .collect()
    }
}

fn inspect_zip_member_extensions(path: &Path, max_entries: usize) -> Option<Vec<String>> {
    let file = File::open(path).ok()?;
    let mut archive = ZipArchive::new(file).ok()?;
    let mut extensions = HashSet::new();

    let limit = archive.len().min(max_entries);
    for index in 0..limit {
        let Ok(entry) = archive.by_index(index) else {
            continue;
        };

        if entry.is_dir() {
            continue;
        }

        let entry_name = entry.name();
        let Some(ext) = Path::new(entry_name).extension().and_then(|s| s.to_str()) else {
            continue;
        };

        let ext_lower = ext.to_ascii_lowercase();

        // Nested archives are not useful for slug classification and add noise.
        if matches!(ext_lower.as_str(), "zip" | "7z" | "rar" | "tar" | "gz") {
            continue;
        }

        extensions.insert(ext_lower);
    }

    if extensions.is_empty() {
        return None;
    }

    Some(extensions.into_iter().collect())
}

fn extract_bracket_tokens(file_name: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut active = false;

    for ch in file_name.chars() {
        match ch {
            '[' | '(' => {
                active = true;
                current.clear();
            }
            ']' | ')' => {
                if active && !current.is_empty() {
                    tokens.push(current.trim().to_ascii_lowercase());
                }
                active = false;
                current.clear();
            }
            _ => {
                if active {
                    current.push(ch);
                }
            }
        }
    }

    tokens
}
