//! The crate graph is the architecture, so it is a test.
//!
//! `20-architecture.md` ("Crates") gives each workspace member the set of other
//! workspace members it may depend on. This test reads `cargo metadata` and
//! asserts that set exactly — no missing edge, no extra one — and that every
//! workspace member appears in the table, so a new crate cannot arrive
//! unclassified.
//!
//! **Which dependency kinds count.** All three: normal, `build-dependencies`
//! and `dev-dependencies`. A dev-dependency is a real edge — it compiles, it
//! can pull a banned transitive crate in, and "the TUI must not link `pi-ai`"
//! is not satisfied by a test binary that does. Including dev-deps is the
//! stricter reading, and the one that keeps `cargo tree` honest; a crate that
//! genuinely needs another member only for its tests gets a row in the table
//! saying so rather than a silent exemption. Cargo forbids cycles among
//! normal deps only, so a dev-dep cycle would be legal and is exactly the kind
//! of thing this table is here to catch.
//!
//! **This file lived in `crates/pirs-protocol/tests/` in phase 0** and moved
//! here in phase 1, now that `pirs` is the crate that depends on all the
//! others. Phase 3 adds `pirs-tui` → `{pirs-protocol, pirs-client}` and, at
//! its end, deletes the `pi-cli` row (D-04); phase 4 deletes the `pi-ext`
//! row. Edit `EXPECTED` and nothing else.
//!
//! `pi-cli` is the phase 1-3 stopgap: `pirs tui` runs the old in-process
//! interactive mode by calling into it, which is why `pirs` depends on a
//! crate the finished architecture does not have.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

/// Each workspace member and the workspace members it may depend on.
///
/// The authority is the "Crates" table in `docs/design/20-architecture.md`.
const EXPECTED: &[(&str, &[&str])] = &[
    ("pi-ai", &[]),
    ("pi-agent", &["pi-ai"]),
    ("pi-ext", &["pi-ai", "pi-agent"]),
    ("pi-cli", &["pi-ai", "pi-agent", "pi-ext"]),
    ("pirs-protocol", &[]),
    ("pirs-server", &["pi-ai", "pi-agent", "pirs-protocol"]),
    ("pirs-client", &["pirs-protocol"]),
    ("pirs", &["pirs-protocol", "pirs-client", "pirs-server", "pi-cli"]),
];

/// The workspace root: two levels up from `crates/pirs`.
fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("crate lives at <workspace>/crates/<name>")
        .to_path_buf()
}

/// `cargo metadata --format-version 1 --no-deps` for this workspace.
fn metadata() -> serde_json::Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(&cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(workspace_root())
        .output()
        .unwrap_or_else(|e| panic!("running `{cargo} metadata` failed: {e}"));
    assert!(
        output.status.success(),
        "`{cargo} metadata` exited with {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata emits JSON")
}

/// Every workspace member with the set of workspace members it depends on,
/// across normal, build and dev dependencies.
fn actual_edges(meta: &serde_json::Value) -> Vec<(String, BTreeSet<String>)> {
    let members: BTreeSet<&str> = meta["workspace_members"]
        .as_array()
        .expect("workspace_members is an array")
        .iter()
        .map(|id| id.as_str().expect("a package id is a string"))
        .collect();

    let packages = meta["packages"].as_array().expect("packages is an array");

    // `--no-deps` already restricts `packages` to the workspace, but filter by
    // id anyway so the test does not depend on that.
    let member_names: BTreeSet<&str> = packages
        .iter()
        .filter(|p| members.contains(p["id"].as_str().unwrap_or_default()))
        .map(|p| p["name"].as_str().expect("a package has a name"))
        .collect();

    let mut edges = Vec::new();
    for package in packages {
        if !members.contains(package["id"].as_str().unwrap_or_default()) {
            continue;
        }
        let name = package["name"].as_str().expect("a package has a name");
        let mut internal = BTreeSet::new();
        for dep in package["dependencies"]
            .as_array()
            .expect("dependencies is an array")
        {
            // `name` is the dependency's real package name even when it is
            // renamed with `package = ...`, which is what we want to match
            // against the workspace members. All kinds count (see the module
            // comment): normal (`kind: null`), `build` and `dev`.
            let dep_name = dep["name"].as_str().expect("a dependency has a name");
            if dep_name != name && member_names.contains(dep_name) {
                internal.insert(dep_name.to_owned());
            }
        }
        edges.push((name.to_owned(), internal));
    }
    edges.sort();
    edges
}

fn show(set: &BTreeSet<String>) -> String {
    if set.is_empty() {
        "{}".to_owned()
    } else {
        format!("{{{}}}", set.iter().cloned().collect::<Vec<_>>().join(", "))
    }
}

#[test]
fn internal_dependencies_match_the_architecture_table() {
    let meta = metadata();
    let actual = actual_edges(&meta);

    let mut failures: Vec<String> = Vec::new();

    for (name, actual_deps) in &actual {
        let Some((_, expected_deps)) = EXPECTED.iter().find(|(n, _)| n == name) else {
            failures.push(format!(
                "workspace member `{name}` is not in EXPECTED: classify it in \
                 crates/pirs/tests/architecture.rs (and in the \"Crates\" \
                 table of docs/design/20-architecture.md) by adding a row naming \
                 the workspace members it may depend on. Its current internal \
                 dependencies are {}.",
                show(actual_deps)
            ));
            continue;
        };
        let expected: BTreeSet<String> = expected_deps.iter().map(|s| (*s).to_owned()).collect();
        if &expected != actual_deps {
            failures.push(format!(
                "workspace member `{name}`: expected internal dependencies {}, \
                 actual {} (missing {}, unexpected {})",
                show(&expected),
                show(actual_deps),
                show(&expected.difference(actual_deps).cloned().collect()),
                show(&actual_deps.difference(&expected).cloned().collect()),
            ));
        }
    }

    let actual_names: BTreeSet<&str> = actual.iter().map(|(n, _)| n.as_str()).collect();
    for (name, _) in EXPECTED {
        if !actual_names.contains(name) {
            failures.push(format!(
                "EXPECTED lists `{name}`, which is not a workspace member: \
                 remove the row, or add the crate to the workspace."
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "the crate graph does not match the architecture table:\n  - {}",
        failures.join("\n  - "),
    );
}

#[test]
fn expected_table_has_no_duplicate_or_unknown_rows() {
    let mut seen = BTreeSet::new();
    let names: BTreeSet<&str> = EXPECTED.iter().map(|(n, _)| *n).collect();
    for (name, deps) in EXPECTED {
        assert!(seen.insert(*name), "EXPECTED lists `{name}` twice");
        for dep in *deps {
            assert!(
                names.contains(dep),
                "EXPECTED row `{name}` names `{dep}`, which is not a row of its own"
            );
            assert_ne!(dep, name, "EXPECTED row `{name}` depends on itself");
        }
    }
}
