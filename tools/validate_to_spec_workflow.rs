use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const REQUIRED_SECTIONS: [&str; 9] = [
    "Problem Statement",
    "Solution",
    "User Stories",
    "Acceptance Criteria",
    "Implementation Decisions",
    "Testing Decisions",
    "Out of Scope",
    "Further Notes",
    "Feature",
];

fn allocate_destination(root: &Path, slug: &str) -> PathBuf {
    for suffix in 1.. {
        let candidate_slug = if suffix == 1 {
            slug.to_owned()
        } else {
            format!("{slug}-{suffix}")
        };
        let candidate = root.join(".scratch").join(candidate_slug).join("spec.md");
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("finite filesystem cannot exhaust the suffix range")
}

fn markdown_files(root: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            markdown_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "md") {
            files.push(path);
        }
    }
}

fn has_heading(content: &str, heading: &str) -> bool {
    content
        .lines()
        .any(|line| line.trim() == format!("## {heading}"))
}

fn validate_spec_quality(content: &str) -> Vec<String> {
    let mut errors = Vec::new();
    for section in REQUIRED_SECTIONS {
        if section == "Feature" {
            if !content.lines().any(|line| line.starts_with("**Feature:**")) {
                errors.push("missing Feature metadata".to_owned());
            }
        } else if !has_heading(content, section) {
            errors.push(format!("missing ## {section} section"));
        }
    }
    for metadata in ["Date", "Status"] {
        if !content
            .lines()
            .any(|line| line.starts_with(&format!("**{metadata}:**")))
        {
            errors.push(format!("missing {metadata} metadata"));
        }
    }
    if !content.contains("**Status:** Draft") {
        errors.push("spec is not Draft".to_owned());
    }
    if !content.lines().any(|line| line.starts_with("1. As an ")) {
        errors.push("missing actor-oriented user stories".to_owned());
    }
    let testing = content
        .split("## Testing Decisions")
        .nth(1)
        .unwrap_or_default();
    if !testing.contains("primary") || !testing.contains("seam") {
        errors.push("testing decisions do not name a primary seam".to_owned());
    }
    errors
}

fn assert_skill_contract(root: &Path) -> Result<(), String> {
    let skill = fs::read_to_string(root.join(".agents/skills/to-spec/SKILL.md"))
        .map_err(|error| format!("read SKILL.md: {error}"))?;
    for phrase in [
        "exactly one local Markdown spec",
        "without an interview",
        "Do NOT interview the user",
        ".scratch/<feature-slug>/spec.md",
        "Do not create a second copy in `docs/`",
        "Never overwrite an existing file",
        "numeric suffix",
        "Do not publish to an issue tracker",
        "stage changes",
        "commit changes",
        "Testing decisions must name one primary",
        "make to-spec-check",
    ] {
        if !skill.contains(phrase) {
            return Err(format!("SKILL.md is missing contract phrase: {phrase}"));
        }
    }

    let metadata = fs::read_to_string(root.join(".agents/skills/to-spec/agents/openai.yaml"))
        .map_err(|error| format!("read openai.yaml: {error}"))?;
    if !metadata.contains("Create a local Markdown feature spec") {
        return Err("openai.yaml does not describe local Markdown feature specs".to_owned());
    }
    Ok(())
}

fn representative_spec() -> &'static str {
    "# Example\n\n**Feature:** example\n**Date:** 2026-07-31\n**Status:** Draft\n\n## Problem Statement\nA problem.\n\n## Solution\nA solution.\n\n## User Stories\n1. As an operator, I want a local draft, so that I can review it.\n\n## Acceptance Criteria\n1. The draft is local.\n\n## Implementation Decisions\n- Keep the workflow local.\n\n## Testing Decisions\nUse one primary application seam to verify the filesystem behavior.\n\n## Out of Scope\n- Remote publication.\n\n## Further Notes\n- None.\n"
}

fn run_fixture() -> Result<(), String> {
    let fixture = env::temp_dir().join(format!("nca-to-spec-validator-{}", std::process::id()));
    let _ = fs::remove_dir_all(&fixture);

    let first = allocate_destination(&fixture, "example");
    if first != fixture.join(".scratch/example/spec.md") {
        return Err(format!("unexpected first destination: {}", first.display()));
    }
    fs::create_dir_all(first.parent().expect("destination parent"))
        .map_err(|error| error.to_string())?;
    let existing = "existing spec must remain byte-for-byte unchanged\n";
    fs::write(&first, existing).map_err(|error| error.to_string())?;
    let before = {
        let mut files = Vec::new();
        markdown_files(&fixture, &mut files);
        files
    };

    let second = allocate_destination(&fixture, "example");
    if second != fixture.join(".scratch/example-2/spec.md") {
        return Err(format!(
            "collision did not receive a numeric suffix: {}",
            second.display()
        ));
    }
    fs::create_dir_all(second.parent().expect("destination parent"))
        .map_err(|error| error.to_string())?;
    fs::write(&second, representative_spec()).map_err(|error| error.to_string())?;
    let after = {
        let mut files = Vec::new();
        markdown_files(&fixture, &mut files);
        files
    };
    if after.len() != before.len() + 1
        || fs::read_to_string(&first).ok().as_deref() != Some(existing)
    {
        return Err(
            "fixture did not preserve existing files and create exactly one output".to_owned(),
        );
    }
    if let Some(error) = validate_spec_quality(representative_spec()).first() {
        return Err(format!(
            "representative spec failed quality validation: {error}"
        ));
    }

    let _ = fs::remove_dir_all(&fixture);
    Ok(())
}

fn main() {
    let root = env::current_dir().expect("current directory");
    if let Err(error) = assert_skill_contract(&root).and_then(|()| run_fixture()) {
        eprintln!("FAIL: {error}");
        std::process::exit(1);
    }
    println!("PASS: to-spec local workflow contract and filesystem fixtures");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_uses_first_unused_numeric_suffix() {
        let root = env::temp_dir().join(format!("nca-to-spec-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".scratch/example")).expect("create destination");
        fs::write(root.join(".scratch/example/spec.md"), "existing")
            .expect("write first collision");
        assert_eq!(
            allocate_destination(&root, "example"),
            root.join(".scratch/example-2/spec.md")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn quality_requires_the_primary_testing_seam() {
        let spec = representative_spec().replace("primary application seam", "one test");
        assert!(!validate_spec_quality(&spec).is_empty());
    }
}
