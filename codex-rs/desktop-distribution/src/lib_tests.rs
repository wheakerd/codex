use pretty_assertions::assert_eq;
use std::fs;
use std::path::Path;

use super::DesktopDistributionError;
use super::ResourceKind;
use super::canonical;
use super::contained_path;

#[test]
fn resolves_strictly_contained_files_and_directories() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("resources");
    let directory = root.join("plugins/demo");
    fs::create_dir_all(&directory).expect("create directory");
    let file = directory.join("hook.json");
    fs::write(&file, "{}").expect("write file");
    let root = canonical(&root, "test resources root").expect("canonical root");

    assert_eq!(
        contained_path(&root, Path::new("plugins/demo"), ResourceKind::Directory)
            .expect("contained directory"),
        canonical(&directory, "test directory").expect("canonical directory")
    );
    assert_eq!(
        contained_path(
            &root,
            Path::new("plugins/demo/hook.json"),
            ResourceKind::File
        )
        .expect("contained file"),
        canonical(&file, "test file").expect("canonical file")
    );
}

#[test]
fn rejects_non_normal_and_wrong_kind_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("resources");
    fs::create_dir_all(root.join("plugins")).expect("create directory");

    for path in ["", ".", "../resources/plugins", "/tmp"] {
        assert!(matches!(
            contained_path(&root, Path::new(path), ResourceKind::Directory),
            Err(DesktopDistributionError::Containment(_))
        ));
    }
    assert!(matches!(
        contained_path(&root, Path::new("plugins"), ResourceKind::File),
        Err(DesktopDistributionError::Containment(_))
    ));
}

#[cfg(unix)]
#[test]
fn rejects_symlink_traversal() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("resources");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&root).expect("create root");
    fs::create_dir_all(&outside).expect("create outside");
    fs::write(outside.join("hook.json"), "{}").expect("write file");
    symlink(&outside, root.join("plugins")).expect("create symlink");

    assert!(matches!(
        contained_path(&root, Path::new("plugins/hook.json"), ResourceKind::File),
        Err(DesktopDistributionError::Containment(_))
    ));
}
