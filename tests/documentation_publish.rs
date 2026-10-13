#![cfg(unix)]

use std::path::Path;

#[test]
fn test_published_documentation_preserves_links_without_modifying_sources() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read(root.join("README.md")).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let output = std::process::Command::new("python3")
        .arg(root.join("scripts/prepare_docs.py"))
        .arg("--output")
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(root.join("README.md")).unwrap(), readme);

    let docs = directory.path().join("docs");
    let examples = std::fs::read_to_string(docs.join("examples.md")).unwrap();
    assert!(examples.contains("](examples/basic/README.md)"));
    let feature = std::fs::read_to_string(docs.join("features/v2-f8-local-execution.md")).unwrap();
    assert!(feature.contains("https://github.com/aiperceivable/apexe/blob/main/src/"));
    let checked = check_internal_links(&docs);
    assert!(
        checked > 100,
        "Only {checked} internal documentation links checked"
    );
}

fn check_internal_links(docs: &Path) -> usize {
    let links = regex::Regex::new(r"\]\(([^\s)]+)").unwrap();
    let mut pending = vec![docs.to_path_buf()];
    let mut checked = 0;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "md") {
                let text = std::fs::read_to_string(&path).unwrap();
                for link in links.captures_iter(&text) {
                    let target = link[1].split('#').next().unwrap();
                    if target.is_empty() || target.contains(':') {
                        continue;
                    }
                    checked += 1;
                    assert!(
                        path.parent().unwrap().join(target).exists(),
                        "{}: {target}",
                        path.display()
                    );
                }
            }
        }
    }
    checked
}
