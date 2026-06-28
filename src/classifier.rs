use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::Path,
};

use anyhow::{Context, Result};
use serde::Deserialize;
use sevenz_rust::Archive;
use zip::ZipArchive;

use crate::model::Confidence;

#[derive(Debug, Clone, Deserialize)]
pub struct PlatformRule {
    pub slug: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(skip)]
    pub rom_extensions: Vec<String>,
    #[serde(default)]
    pub folder_hints: Vec<String>,
    #[serde(default)]
    pub dat_tokens: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PlatformRomTypeRule {
    slug: String,
    #[serde(default)]
    rom_extensions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PlatformRomTypeIndex {
    #[serde(default)]
    archive_extensions: Vec<String>,
    platforms: Vec<PlatformRomTypeRule>,
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
    archive_extensions: HashSet<String>,
}

impl Classifier {
    pub fn from_embedded() -> Result<Self> {
        let raw = include_str!("../assets/platform_index.json");
        let mut parsed: PlatformIndex =
            serde_json::from_str(raw).context("Failed to parse embedded platform index")?;
        let rom_type_raw = include_str!("../assets/platform_rom_file_types.json");
        let rom_types: PlatformRomTypeIndex = serde_json::from_str(rom_type_raw)
            .context("Failed to parse embedded platform ROM file types")?;

        let archive_extensions: HashSet<String> = rom_types
            .archive_extensions
            .into_iter()
            .map(|ext| ext.to_ascii_lowercase())
            .collect();

        let rom_extensions_by_slug: HashMap<String, Vec<String>> = rom_types
            .platforms
            .into_iter()
            .map(|rule| {
                (
                    rule.slug,
                    rule.rom_extensions
                        .into_iter()
                        .map(|ext| ext.to_ascii_lowercase())
                        .collect(),
                )
            })
            .collect();

        for rule in &mut parsed.platforms {
            rule.rom_extensions = rom_extensions_by_slug
                .get(&rule.slug)
                .cloned()
                .unwrap_or_else(|| {
                    rule.extensions
                        .iter()
                        .filter(|ext| !archive_extensions.contains(&ext.to_ascii_lowercase()))
                        .map(|ext| ext.to_ascii_lowercase())
                        .collect()
                });
        }

        Ok(Self {
            rules: parsed.platforms,
            archive_extensions,
        })
    }

    pub fn classify(&self, path: &Path) -> Classification {
        let extension = path
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        let archive_member_extensions = inspect_archive_member_extensions(
            path,
            &extension,
            &self.archive_extensions,
            200,
        );

        let file_name = path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let normalized_file_name = normalize_text_for_matching(&file_name);

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
                    .rom_extensions
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

            if rule.aliases.iter().any(|alias| {
                let alias_norm = normalize_text_for_matching(alias);
                if alias_norm.is_empty() {
                    return false;
                }

                let haystack = format!(" {} ", normalized_file_name);
                let needle = format!(" {} ", alias_norm);
                haystack.contains(&needle)
            }) {
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

            if let Some(member_exts) = &archive_member_extensions {
                if let Some(found_ext) = member_exts.iter().find(|member_ext| {
                    rule.rom_extensions
                        .iter()
                        .any(|ext| ext.eq_ignore_ascii_case(member_ext))
                }) {
                    score += 90;
                    reasons.push(format!("archive member extension .{found_ext}"));
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

        if tied {
            return Classification {
                platform_slug: None,
                confidence: Confidence::Ambiguous,
                reason: String::from("multiple platforms matched with equal score"),
            };
        }

        if let Some(slug) = best_slug {
            let confidence = if best_score >= 100 {
                Confidence::Exact
            } else {
                Confidence::High
            };

            return Classification {
                platform_slug: Some(slug.to_owned()),
                confidence,
                reason: best_reason,
            };
        }

        Classification {
            platform_slug: None,
            confidence: Confidence::Unknown,
            reason: String::from("no platform match"),
        }
    }

    pub fn allowed_extensions(&self) -> HashSet<String> {
        let mut allowed: HashSet<String> = self
            .rules
            .iter()
            .flat_map(|rule| rule.rom_extensions.iter())
            .map(|ext| ext.to_ascii_lowercase())
            .collect();
        allowed.extend(self.archive_extensions.iter().cloned());
        allowed
    }
}

fn normalize_text_for_matching(input: &str) -> String {
    let mut out = String::with_capacity(input.len());

    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(' ');
        }
    }

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn inspect_archive_member_extensions(
    path: &Path,
    extension: &str,
    archive_extensions: &HashSet<String>,
    max_entries: usize,
) -> Option<Vec<String>> {
    if !archive_extensions.contains(extension) {
        return None;
    }

    match extension {
        "zip" => inspect_zip_member_extensions(path, max_entries),
        "7z" => inspect_7z_member_extensions(path, max_entries),
        _ => None,
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

fn inspect_7z_member_extensions(path: &Path, max_entries: usize) -> Option<Vec<String>> {
    let archive = Archive::open(path).ok()?;
    let mut extensions = HashSet::new();

    for entry in archive.files.iter().take(max_entries) {
        if entry.is_directory() {
            continue;
        }

        let Some(ext) = Path::new(entry.name()).extension().and_then(|s| s.to_str()) else {
            continue;
        };

        let ext_lower = ext.to_ascii_lowercase();
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        fs::File,
        io::Write,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use sevenz_rust::compress_to_path;
    use zip::write::SimpleFileOptions;

    use super::Classifier;
    use crate::model::Confidence;

    #[test]
    fn classifies_zip_by_member_rom_extension() {
        let temp_dir = temp_test_dir("zip");
        let archive_path = temp_dir.join("Disney.zip");
        write_zip_archive(&archive_path, "Disney.gba", b"test-rom");

        let classifier = Classifier::from_embedded().expect("classifier");
        let classification = classifier.classify(&archive_path);

        assert_eq!(classification.platform_slug.as_deref(), Some("gba"));
        assert_eq!(classification.confidence, Confidence::High);

        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn classifies_7z_by_member_rom_extension() {
        let temp_dir = temp_test_dir("7z");
        let source_dir = temp_dir.join("src");
        let archive_path = temp_dir.join("Disney.7z");
        fs::create_dir_all(&source_dir).expect("create source dir");
        fs::write(source_dir.join("Disney.gba"), b"test-rom").expect("write rom file");
        compress_to_path(&source_dir, &archive_path).expect("create 7z archive");

        let classifier = Classifier::from_embedded().expect("classifier");
        let classification = classifier.classify(&archive_path);

        assert_eq!(classification.platform_slug.as_deref(), Some("gba"));
        assert_eq!(classification.confidence, Confidence::High);

        let _ = fs::remove_dir_all(temp_dir);
    }

    fn temp_test_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("move_roms_{label}_{unique}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_zip_archive(path: &Path, entry_name: &str, contents: &[u8]) {
        let file = File::create(path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(entry_name, SimpleFileOptions::default())
            .expect("start zip entry");
        zip.write_all(contents).expect("write zip contents");
        zip.finish().expect("finish zip");
    }
}
