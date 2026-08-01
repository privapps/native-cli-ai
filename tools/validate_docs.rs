use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn collect_markdown(path: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };

    for entry in entries.flatten() {
        let entry_path = entry.path();
        if entry_path.is_dir() {
            collect_markdown(&entry_path, files);
        } else if entry_path
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            files.push(entry_path);
        }
    }
}

fn heading_anchors(content: &str) -> HashMap<String, usize> {
    let mut anchors = HashMap::new();
    for line in content.lines() {
        let Some(heading) = line.strip_prefix('#') else {
            continue;
        };
        let heading = heading.trim_start_matches('#');
        if !heading.starts_with(' ') {
            continue;
        }

        let mut anchor = String::new();
        let mut pending_dash = false;
        for character in heading.trim().chars() {
            if character.is_alphanumeric() {
                if pending_dash && !anchor.is_empty() {
                    anchor.push('-');
                }
                anchor.push(character.to_ascii_lowercase());
                pending_dash = false;
            } else if !anchor.is_empty() && matches!(character, ' ' | '-' | '_') {
                pending_dash = true;
            }
        }
        if anchor.is_empty() {
            continue;
        }

        let occurrence = anchors.get(&anchor).copied().unwrap_or(0);
        if occurrence > 0 {
            let disambiguated = format!("{anchor}-{occurrence}");
            anchors.insert(disambiguated, 1);
        }
        anchors.insert(anchor, occurrence + 1);
    }
    anchors
}

fn is_canonical_design_document(path: &Path) -> bool {
    if !path.is_file() || path.extension().is_none_or(|extension| extension != "md") {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    let Some(parent_name) = parent.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(parent_name, "plans" | "specs")
        && parent
            .parent()
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| name == "design")
        && path.file_name().is_some_and(|name| name != "README.md")
}

fn has_required_metadata(content: &str) -> bool {
    ["Type", "Status", "Date", "Related", "Last verified"]
        .into_iter()
        .all(|field| {
            content
                .lines()
                .take(20)
                .any(|line| line.trim_start().starts_with(&format!("**{field}:**")))
        })
}

fn check_design_metadata(root: &Path, errors: &mut Vec<String>) {
    for directory in ["docs/design/plans", "docs/design/specs"] {
        let path = root.join(directory);
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let file = entry.path();
            if !is_canonical_design_document(&file) {
                continue;
            }
            match fs::read_to_string(&file) {
                Ok(content) if has_required_metadata(&content) => {}
                Ok(_) => errors.push(format!(
                    "{}: missing required lifecycle metadata (Type, Status, Date, Related, Last verified)",
                    file.display()
                )),
                Err(error) => errors.push(format!(
                    "{}: could not read lifecycle metadata: {error}",
                    file.display()
                )),
            }
        }
    }
}

fn check_index_coverage(root: &Path, errors: &mut Vec<String>) {
    for directory in ["docs/design/plans", "docs/design/specs"] {
        let index = root.join(directory).join("README.md");
        let Ok(index_content) = fs::read_to_string(&index) else {
            errors.push(format!(
                "{}: missing canonical design index",
                index.display()
            ));
            continue;
        };
        let Ok(entries) = fs::read_dir(root.join(directory)) else {
            continue;
        };
        for entry in entries.flatten() {
            let file = entry.path();
            if !is_canonical_design_document(&file) {
                continue;
            }
            let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !index_content.contains(&format!("]({name})")) {
                errors.push(format!(
                    "{}: canonical document is not linked from {}",
                    file.display(),
                    index.display()
                ));
            }
        }
    }
}

fn check_compatibility_pages(root: &Path, errors: &mut Vec<String>) {
    let legacy_roots = [
        "docs/documentation",
        "docs/superpowers/specs",
        "docs/superpowers/plans",
        "docs/plans",
    ];
    for directory in legacy_roots {
        let path = root.join(directory);
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let file = entry.path();
            if file.file_name().is_some_and(|name| name == "README.md")
                || file.extension().is_none_or(|extension| extension != "md")
            {
                continue;
            }
            let Ok(content) = fs::read_to_string(&file) else {
                errors.push(format!(
                    "{}: compatibility page is unreadable",
                    file.display()
                ));
                continue;
            };
            let lower = content.to_ascii_lowercase();
            if !lower.contains("moved") && !lower.contains("compatibility") {
                errors.push(format!(
                    "{}: legacy documentation must be an explicit compatibility page",
                    file.display()
                ));
            }
            if !has_existing_canonical_target(root, &file, &content) {
                errors.push(format!(
                    "{}: compatibility page must link to an existing canonical documentation target",
                    file.display()
                ));
            }
        }
    }
}

fn has_existing_canonical_target(root: &Path, path: &Path, content: &str) -> bool {
    let canonical_roots: Vec<PathBuf> = [
        root.join("docs/user"),
        root.join("docs/product"),
        root.join("docs/reference"),
        root.join("docs/design"),
        root.join("docs/research"),
    ]
    .into_iter()
    .filter_map(|path| path.canonicalize().ok())
    .collect();
    let mut remaining = content;
    while let Some(link_start) = remaining.find("](") {
        let link_body = &remaining[link_start + 2..];
        let Some(link_end) = link_body.find(')') else {
            break;
        };
        let target = link_body[..link_end]
            .trim()
            .strip_prefix('<')
            .and_then(|target| target.strip_suffix('>'))
            .unwrap_or(link_body[..link_end].trim())
            .split_whitespace()
            .next()
            .unwrap_or_default();
        let (file_target, _) = target.split_once('#').unwrap_or((target, ""));
        if !file_target.is_empty()
            && !file_target.starts_with("http://")
            && !file_target.starts_with("https://")
            && !file_target.starts_with("mailto:")
        {
            let resolved = path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(file_target);
            if let Ok(resolved) = resolved.canonicalize()
                && resolved.is_file()
                && canonical_roots
                    .iter()
                    .any(|canonical_root| resolved.starts_with(canonical_root))
            {
                return true;
            }
        }
        remaining = &link_body[link_end + 1..];
    }
    false
}

fn check_active_lifecycle(root: &Path, errors: &mut Vec<String>) {
    for directory in ["docs/design/plans", "docs/design/specs"] {
        let Ok(entries) = fs::read_dir(root.join(directory)) else {
            continue;
        };
        for entry in entries.flatten() {
            let file = entry.path();
            if !is_canonical_design_document(&file) {
                continue;
            }
            let Ok(content) = fs::read_to_string(&file) else {
                continue;
            };
            let status = content
                .lines()
                .take(20)
                .find_map(|line| line.trim_start().strip_prefix("**Status:**"))
                .unwrap_or_default()
                .to_ascii_lowercase();
            if status.contains("historical") || status.contains("archived") {
                errors.push(format!(
                    "{}: active design directory document has historical/archive status",
                    file.display()
                ));
            }
        }
    }
}

fn check_duplicate_active_documents(root: &Path, errors: &mut Vec<String>) {
    for directory in ["docs/design/plans", "docs/design/specs"] {
        let Ok(entries) = fs::read_dir(root.join(directory)) else {
            continue;
        };
        let mut titles = HashMap::new();
        for entry in entries.flatten() {
            let file = entry.path();
            if !is_canonical_design_document(&file) {
                continue;
            }
            let Ok(content) = fs::read_to_string(&file) else {
                continue;
            };
            let Some(title) = content
                .lines()
                .find_map(|line| line.strip_prefix("# "))
                .map(str::trim)
            else {
                continue;
            };
            if let Some(previous) = titles.insert(title.to_owned(), file.clone()) {
                errors.push(format!(
                    "{}: duplicate active document title also used by {}",
                    file.display(),
                    previous.display()
                ));
            }
        }
    }
}

fn check_markdown_links(path: &Path, content: &str, errors: &mut Vec<String>) {
    let mut remaining = content;
    while let Some(link_start) = remaining.find("](") {
        let link_body = &remaining[link_start + 2..];
        let Some(link_end) = link_body.find(')') else {
            break;
        };
        let raw_target = link_body[..link_end].trim();
        let target = raw_target
            .strip_prefix('<')
            .and_then(|target| target.strip_suffix('>'))
            .unwrap_or(raw_target)
            .split_whitespace()
            .next()
            .unwrap_or_default();

        if !target.is_empty()
            && !target.starts_with('#')
            && !target.starts_with("http://")
            && !target.starts_with("https://")
            && !target.starts_with("mailto:")
        {
            let (file_target, fragment) = target.split_once('#').unwrap_or((target, ""));
            let resolved = path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(file_target);
            let exists = resolved.is_file()
                || (resolved.is_dir()
                    && (resolved.join("index.md").is_file()
                        || resolved.join("README.md").is_file()));
            if !exists {
                errors.push(format!(
                    "{}: broken relative link `{target}`",
                    path.display()
                ));
            } else if !fragment.is_empty() {
                let anchor_file = if file_target.is_empty() {
                    path.to_path_buf()
                } else {
                    resolved
                };
                match fs::read_to_string(&anchor_file) {
                    Ok(content) if heading_anchors(&content).contains_key(fragment) => {}
                    Ok(_) => errors.push(format!(
                        "{}: broken anchor in relative link `{target}`",
                        path.display()
                    )),
                    Err(error) => errors.push(format!(
                        "{}: could not read anchor target `{target}`: {error}",
                        path.display()
                    )),
                }
            }
        }

        remaining = &link_body[link_end + 1..];
    }
}

fn cli_surface() -> Result<(Vec<String>, Vec<String>), String> {
    let output = Command::new("cargo")
        .args(["run", "--quiet", "-p", "nca-cli", "--", "--help"])
        .output()
        .map_err(|error| format!("could not run nca help: {error}"))?;
    if !output.status.success() {
        return Err("cargo run -p nca-cli -- --help failed".into());
    }

    let help = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    Ok(parse_cli_surface(&help))
}

fn parse_cli_surface(help: &str) -> (Vec<String>, Vec<String>) {
    let mut in_commands = false;
    let mut in_options = false;
    let mut commands = Vec::new();
    let mut options = Vec::new();
    for line in help.lines() {
        if line.trim() == "Commands:" {
            in_commands = true;
            in_options = false;
            continue;
        }
        if in_commands && line.trim() == "Options:" {
            in_commands = false;
            in_options = true;
            continue;
        }
        if in_options && line.trim().is_empty() {
            continue;
        }
        if in_commands {
            if let Some(command) = line.split_whitespace().next() {
                commands.push(command.to_owned());
            }
        } else if in_options {
            for token in line.split_whitespace() {
                let token = token.trim_end_matches(',');
                if token.starts_with("--") {
                    options.push(token.to_owned());
                    break;
                }
            }
        }
        if in_options && line.trim() == "-h, --help" {
            break;
        }
    }
    (commands, options)
}

fn cli_reference_contains(content: &str, item: &str) -> bool {
    content.lines().any(|line| {
        line.split_whitespace()
            .any(|token| token.trim_end_matches(',') == item)
    })
}

fn main() {
    let root = env::current_dir().expect("could not determine repository root");
    let required = [
        "docs/README.md",
        "docs/user/index.md",
        "docs/product/index.md",
        "docs/reference/index.md",
        "docs/design/index.md",
        "docs/research/index.md",
        "docs/reference/cli-reference.md",
    ];
    let mut errors = Vec::new();

    for relative in required {
        if !root.join(relative).is_file() {
            errors.push(format!("missing required documentation file: {relative}"));
        }
    }

    let mut markdown = Vec::new();
    collect_markdown(&root.join("docs"), &mut markdown);
    for path in &markdown {
        if let Ok(content) = fs::read_to_string(path) {
            check_markdown_links(path, &content, &mut errors);
        }
    }

    for legacy_dir in ["docs/plans", "docs/superpowers/plans"] {
        let path = root.join(legacy_dir);
        if path.is_dir() {
            let non_readmes = fs::read_dir(&path)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| {
                    entry
                        .path()
                        .file_name()
                        .is_some_and(|name| name != "README.md")
                })
                .count();
            if non_readmes > 0 {
                errors.push(format!(
                    "legacy directory contains non-README content: {legacy_dir}"
                ));
            }
        }
    }

    check_design_metadata(&root, &mut errors);
    check_index_coverage(&root, &mut errors);
    check_compatibility_pages(&root, &mut errors);
    check_active_lifecycle(&root, &mut errors);
    check_duplicate_active_documents(&root, &mut errors);

    let cli_reference = root.join("docs/reference/cli-reference.md");
    if let Ok(content) = fs::read_to_string(cli_reference) {
        match cli_surface() {
            Ok((commands, options)) => {
                for command in commands {
                    if !cli_reference_contains(&content, &command) {
                        errors.push(format!("CLI reference is missing command `{command}`"));
                    }
                }
                for option in options {
                    if !cli_reference_contains(&content, &option) {
                        errors.push(format!("CLI reference is missing option `{option}`"));
                    }
                }
            }
            Err(error) => errors.push(error),
        }
    }

    if errors.is_empty() {
        println!(
            "documentation validation passed ({} Markdown files)",
            markdown.len()
        );
    } else {
        for error in errors {
            eprintln!("error: {error}");
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root(name: &str) -> PathBuf {
        let root =
            env::temp_dir().join(format!("nca-doc-validation-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create fixture root");
        root
    }

    #[test]
    fn heading_anchors_match_markdown_links() {
        let anchors = heading_anchors("# Overview\n## MCP Servers\n## MCP Servers\n");
        assert!(anchors.contains_key("overview"));
        assert!(anchors.contains_key("mcp-servers"));
        assert!(anchors.contains_key("mcp-servers-1"));
    }

    #[test]
    fn broken_anchor_is_reported() {
        let root = fixture_root("anchor");
        let source = root.join("source.md");
        let target = root.join("target.md");
        fs::write(&target, "# Existing section\n").expect("write target");
        let mut errors = Vec::new();
        check_markdown_links(&source, "[target](target.md#missing-section)", &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("broken anchor"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lifecycle_metadata_requires_all_fields() {
        assert!(!has_required_metadata(
            "**Type:** plan\n**Status:** Draft\n"
        ));
        assert!(has_required_metadata(
            "**Type:** plan\n**Status:** Draft\n**Date:** 2026-07-31\n**Related:** None\n**Last verified:** 2026-07-31\n"
        ));
    }

    #[test]
    fn index_and_compatibility_failures_are_reported() {
        let root = fixture_root("structure");
        let plans = root.join("docs/design/plans");
        let specs = root.join("docs/design/specs");
        let legacy = root.join("docs/documentation");
        fs::create_dir_all(&plans).expect("create plans");
        fs::create_dir_all(&specs).expect("create specs");
        fs::create_dir_all(&legacy).expect("create legacy docs");
        fs::write(plans.join("README.md"), "# Plans\n").expect("write plan index");
        fs::write(specs.join("README.md"), "# Specs\n").expect("write spec index");
        fs::write(
            plans.join("missing-metadata.md"),
            "# Plan\n\nNo metadata.\n",
        )
        .expect("write plan");
        fs::write(plans.join("duplicate.md"), "# Plan\n").expect("write duplicate plan");
        fs::write(legacy.join("old.md"), "# Old page\n").expect("write legacy page");

        let mut errors = Vec::new();
        check_design_metadata(&root, &mut errors);
        check_index_coverage(&root, &mut errors);
        check_compatibility_pages(&root, &mut errors);
        check_duplicate_active_documents(&root, &mut errors);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("lifecycle metadata"))
        );
        assert!(errors.iter().any(|error| error.contains("not linked")));
        assert!(
            errors
                .iter()
                .any(|error| error.contains("compatibility page"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("duplicate active"))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn compatibility_pages_require_existing_canonical_targets() {
        let root = fixture_root("compatibility-target");
        let legacy = root.join("docs/documentation");
        fs::create_dir_all(&legacy).expect("create legacy docs");
        fs::write(
            legacy.join("old.md"),
            "# Old documentation moved\n\nSee the [replacement](../missing.md).\n",
        )
        .expect("write legacy page");

        let mut errors = Vec::new();
        check_compatibility_pages(&root, &mut errors);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("existing canonical"))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cli_reference_checks_options_as_well_as_commands() {
        let help = "Commands:\n  run  Run a task\n  help  Show help\nOptions:\n  -p, --prompt <PROMPT>\n      --json\n  -h, --help\n";
        assert_eq!(
            parse_cli_surface(help),
            (
                vec!["run".to_owned(), "help".to_owned()],
                vec![
                    "--prompt".to_owned(),
                    "--json".to_owned(),
                    "--help".to_owned()
                ]
            )
        );
        let reference = "```text\n  run\n  help\n  -p, --prompt <PROMPT>\n      --help\n```";
        assert!(cli_reference_contains(reference, "run"));
        assert!(cli_reference_contains(reference, "--prompt"));
        assert!(!cli_reference_contains(reference, "--json"));
    }

    #[test]
    fn active_documents_reject_historical_statuses() {
        let root = fixture_root("active-lifecycle");
        let plans = root.join("docs/design/plans");
        fs::create_dir_all(&plans).expect("create plans");
        fs::write(
            plans.join("legacy.md"),
            "# Legacy\n\n**Type:** plan\n**Status:** Historical plan\n**Date:** 2026-01-01\n**Related:** None\n**Last verified:** 2026-07-31\n",
        )
        .expect("write legacy plan");
        let mut errors = Vec::new();
        check_active_lifecycle(&root, &mut errors);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("historical/archive"))
        );
        let _ = fs::remove_dir_all(root);
    }
}
