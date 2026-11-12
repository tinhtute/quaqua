//! Determinism guard: verifies that `Workflow` implementations do not directly access
//! `std::fs`, `std::process`, or `std::net`.

use std::path::Path;

/// A detected violation of the workflow determinism rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeterminismViolation {
    pub file: String,
    pub line: usize,
    pub forbidden_pattern: String,
    pub snippet: String,
}

impl std::fmt::Display for DeterminismViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: forbidden non-deterministic access `{}` in Workflow impl: `{}`",
            self.file, self.line, self.forbidden_pattern, self.snippet
        )
    }
}

/// Scans source text for `impl ... Workflow for ...` blocks and checks for forbidden I/O tokens.
pub fn scan_source_for_determinism_violations(
    source: &str,
    file_label: &str,
) -> Vec<DeterminismViolation> {
    let mut violations = Vec::new();
    let forbidden_patterns = ["std::fs", "std::process", "std::net"];

    let mut in_workflow_impl = false;
    let mut brace_depth: i32 = 0;

    for (zero_idx, line) in source.lines().enumerate() {
        let line_num = zero_idx + 1;
        let trimmed = line.trim();

        // Check for start of an impl ... Workflow for block
        if !in_workflow_impl {
            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
                continue;
            }
            if trimmed.contains("impl") && trimmed.contains("Workflow for") {
                in_workflow_impl = true;
                brace_depth = 0;
            }
        }

        if in_workflow_impl {
            // Count brace depth changes
            // Note: simple brace counting handles standard Rust code formatting
            for ch in line.chars() {
                if ch == '{' {
                    brace_depth += 1;
                } else if ch == '}' {
                    brace_depth -= 1;
                }
            }

            // Check for forbidden tokens (ignoring comment lines)
            if !trimmed.starts_with("//") && !trimmed.starts_with("/*") && !trimmed.starts_with('*')
            {
                for &pattern in &forbidden_patterns {
                    if line.contains(pattern) {
                        violations.push(DeterminismViolation {
                            file: file_label.to_string(),
                            line: line_num,
                            forbidden_pattern: pattern.to_string(),
                            snippet: trimmed.to_string(),
                        });
                    }
                }
            }

            // If we have entered the block and now returned to depth 0, the impl block has ended
            if brace_depth <= 0 && line.contains('}') {
                in_workflow_impl = false;
                brace_depth = 0;
            }
        }
    }

    violations
}

/// Recursively scans all `.rs` files in the given directory.
pub fn scan_directory_for_determinism_violations(
    dir: &Path,
) -> Result<Vec<DeterminismViolation>, String> {
    let mut violations = Vec::new();
    scan_dir_recursive(dir, &mut violations)?;
    Ok(violations)
}

fn scan_dir_recursive(dir: &Path, out: &mut Vec<DeterminismViolation>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read dir {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry error: {e}"))?;
        let path = entry.path();
        if path.is_dir() {
            scan_dir_recursive(&path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read file {}: {e}", path.display()))?;
            let mut file_violations =
                scan_source_for_determinism_violations(&content, &path.to_string_lossy());
            out.append(&mut file_violations);
        }
    }
    Ok(())
}
