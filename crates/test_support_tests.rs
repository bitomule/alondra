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
