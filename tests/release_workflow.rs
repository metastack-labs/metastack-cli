use std::fs;
use std::path::PathBuf;

#[test]
fn workflow_dispatch_builds_the_requested_tag_ref() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workflow = fs::read_to_string(repo_root.join(".github/workflows/release.yml"))
        .expect("release workflow should be readable");

    assert!(
        workflow.contains("ref: refs/tags/${{ needs.metadata.outputs.tag }}"),
        "expected workflow_dispatch builds to check out the requested tag ref"
    );
}

#[test]
fn workflow_enforces_tag_and_package_version_alignment() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workflow = fs::read_to_string(repo_root.join(".github/workflows/release.yml"))
        .expect("release workflow should be readable");

    assert!(
        workflow.contains("--expect-version \"${{ needs.metadata.outputs.version }}\""),
        "expected release packaging to enforce the resolved tag version"
    );
}
