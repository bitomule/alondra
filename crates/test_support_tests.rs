mod test_support;
use test_support::Directory;

#[test]
fn removes_written_files_when_the_scope_ends() {
    let path;
    {
        let directory = Directory::new();
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("fixture"), b"test data").unwrap();
        path = directory.to_path_buf();
    }
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());
}

#[test]
fn removes_written_files_when_a_test_panics() {
    let directory = Directory::new();
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("fixture"), b"test data").unwrap();
    let path = directory.to_path_buf();
    let result = std::panic::catch_unwind(move || {
        let _directory = directory;
        panic!("failed test body");
    });
    assert!(result.is_err());
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());
}

#[cfg(unix)]
fn read_only_tree(directory: &Directory) {
    use std::os::unix::fs::PermissionsExt;
    let locked = directory.join("cache/external/rules");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("BUILD"), b"fixture").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    std::fs::set_permissions(directory.join("cache"), std::fs::Permissions::from_mode(0o500)).unwrap();
}

#[cfg(unix)]
#[test]
fn removes_read_only_folders_when_the_scope_ends() {
    let path;
    {
        let directory = Directory::new();
        read_only_tree(&directory);
        path = directory.to_path_buf();
    }
    assert!(!path.parent().unwrap().exists());
}

#[cfg(unix)]
#[test]
fn removes_read_only_folders_when_a_test_panics() {
    let directory = Directory::new();
    read_only_tree(&directory);
    let path = directory.to_path_buf();
    let result = std::panic::catch_unwind(move || {
        let _directory = directory;
        panic!("failed test body");
    });
    assert!(result.is_err());
    assert!(!path.parent().unwrap().exists());
}
