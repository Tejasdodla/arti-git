use assert_cmd::prelude::*; // Add methods on commands
use predicates::prelude::*; // Used for writing assertions
use std::process::Command; // Run programs
use assert_fs::prelude::*; // Create temp files/dirs
use assert_fs::TempDir;
use std::path::Path; // For path manipulation

/// Sets up a temporary directory with an initialized arti-git repository.
fn setup_init_repo() -> Result<TempDir, Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();
    let mut cmd = Command::cargo_bin("arti-git")?;
    cmd.arg("init")
       .arg(repo_path)
       .assert()
       .success();
    Ok(temp_dir)
}

/// Sets up a temporary directory with an initialized bare arti-git repository.
fn setup_init_bare_repo() -> Result<TempDir, Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();
    let mut cmd = Command::cargo_bin("arti-git")?;
    cmd.arg("init")
       .arg("--bare") // Add the bare flag
       .arg(repo_path)
       .assert()
       .success();
    Ok(temp_dir)
}

/// Helper to run git commands in a specific directory
fn run_git_cmd(args: &[&str], cwd: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "Git command failed: {:?}\nStdout: {}\nStderr: {}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ).into());
    }
    Ok(())
}

#[test]
fn test_init_command() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?; // Use basic temp dir for init test
    let repo_path = temp_dir.path();

    let mut cmd = Command::cargo_bin("arti-git")?;

    cmd.arg("init")
       .arg(repo_path)
       .assert()
       .success()
       .stdout(predicate::str::contains("Initialized empty Arti-Git repository"));

    // Verify that the .git directory was created
    temp_dir.child(".git").assert(predicate::path::is_dir());
    // Verify some essential files/dirs inside .git
    temp_dir.child(".git/HEAD").assert(predicate::path::is_file());
    temp_dir.child(".git/config").assert(predicate::path::is_file());
    temp_dir.child(".git/objects").assert(predicate::path::is_dir());
    temp_dir.child(".git/refs").assert(predicate::path::is_dir());

    Ok(())
}

#[test]
fn test_add_commit() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = setup_init_repo()?; // Use helper to init repo
    let repo_path = temp_dir.path();

    // Repo is already initialized by setup_init_repo()

    // 2. Create a file
    let file_name = "test.txt";
    temp_dir.child(file_name)
        .write_str("Hello, Arti-Git!")?;

    // 3. Add the file
    let mut add_cmd = Command::cargo_bin("arti-git")?;
    add_cmd.current_dir(repo_path) // Run 'add' from within the repo dir
           .arg("add")
           .arg(file_name)
           .assert()
           .success();
           // TODO: Assert stdout/stderr if 'add' produces output

    // 4. Commit the file
    let mut commit_cmd = Command::cargo_bin("arti-git")?;
    commit_cmd.current_dir(repo_path) // Run 'commit' from within the repo dir
              .arg("commit")
              .arg("-m")
              .arg("Initial commit")
              .assert()
              .success();
              // TODO: Assert stdout contains commit hash/summary

    // TODO: Add verification that the commit actually exists (e.g., check HEAD ref, object exists)

    Ok(())
}

#[test]
fn test_push_basic() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Setup local and remote repos
    let local_repo_dir = setup_init_repo()?;
    let remote_repo_dir = setup_init_bare_repo()?;
    let local_path = local_repo_dir.path();
    let remote_path_str = remote_repo_dir.path().to_str().expect("Remote path is not valid UTF-8");

    // 2. Configure remote 'origin' in local repo using standard git
    run_git_cmd(&["remote", "add", "origin", remote_path_str], local_path)?;

    // 3. Create and commit a file in local repo
    let file_name = "data.txt";
    local_repo_dir.child(file_name).write_str("Push me!")?;
    let mut add_cmd = Command::cargo_bin("arti-git")?;
    add_cmd.current_dir(local_path).arg("add").arg(file_name).assert().success();
    let mut commit_cmd = Command::cargo_bin("arti-git")?;
    commit_cmd.current_dir(local_path).arg("commit").arg("-m").arg("Commit to push").assert().success();

    // 4. Push to remote
    let mut push_cmd = Command::cargo_bin("arti-git")?;
    push_cmd.current_dir(local_path)
            .arg("push")
            .arg("origin")
            .arg("main") // Assuming default branch is main
            .assert()
            .success();
            // TODO: Assert stdout indicates successful push

    // 5. Verify commit exists on remote
    // TODO: Implement a way to check refs/objects in the bare remote repo
    // Example: Check if remote_repo_dir.path().join("refs/heads/main") exists
    remote_repo_dir.child("refs/heads/main").assert(predicate::path::is_file());

    Ok(())
}


#[test]
fn test_pull_fast_forward() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Setup local and remote repos
    let local_repo_dir = setup_init_repo()?;
    let remote_repo_dir = setup_init_bare_repo()?;
    let local_path = local_repo_dir.path();
    let remote_path_str = remote_repo_dir.path().to_str().expect("Remote path is not valid UTF-8");

    // 2. Configure remote and make initial commit/push
    run_git_cmd(&["remote", "add", "origin", remote_path_str], local_path)?;
    let file1_name = "file1.txt";
    local_repo_dir.child(file1_name).write_str("Initial content")?;
    run_git_cmd(&["add", file1_name], local_path)?;
    run_git_cmd(&["commit", "-m", "Commit 1"], local_path)?;
    let mut push_cmd = Command::cargo_bin("arti-git")?;
    push_cmd.current_dir(local_path).arg("push").arg("origin").arg("main").assert().success();

    // 3. Simulate a commit happening on the remote
    let remote_clone_dir = setup_test_dir(); // Use basic temp dir helper
    run_git_cmd(&["clone", remote_path_str, "."], remote_clone_dir.path())?;
    let file2_name = "file2.txt";
    remote_clone_dir.child(file2_name).write_str("Remote content")?;
    run_git_cmd(&["add", file2_name], remote_clone_dir.path())?;
    run_git_cmd(&["commit", "-m", "Commit 2"], remote_clone_dir.path())?;
    run_git_cmd(&["push", "origin", "main"], remote_clone_dir.path())?;

    // 4. Pull from the original local repo
    let mut pull_cmd = Command::cargo_bin("arti-git")?;
    pull_cmd.current_dir(local_path)
            .arg("pull")
            .arg("origin")
            .arg("main")
            .assert()
            .success();
            // TODO: Assert stdout indicates fast-forward or successful merge/checkout

    // 5. Verify both files exist locally
    local_repo_dir.child(file1_name).assert(predicate::path::exists());
    local_repo_dir.child(file2_name).assert(predicate::path::exists());

    Ok(())
}


#[test]
fn test_pull_merge_conflict() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Setup local and remote repos
    let local_repo_dir = setup_init_repo()?;
    let remote_repo_dir = setup_init_bare_repo()?;
    let local_path = local_repo_dir.path();
    let remote_path_str = remote_repo_dir.path().to_str().expect("Remote path is not valid UTF-8");

    // 2. Configure remote and make base commit/push
    run_git_cmd(&["remote", "add", "origin", remote_path_str], local_path)?;
    let file_name = "conflict.txt";
    local_repo_dir.child(file_name).write_str("Base content")?;
    run_git_cmd(&["add", file_name], local_path)?;
    run_git_cmd(&["commit", "-m", "Base commit"], local_path)?;
    let mut push_cmd = Command::cargo_bin("arti-git")?;
    push_cmd.current_dir(local_path).arg("push").arg("origin").arg("main").assert().success();

    // 3. Create conflicting commit locally
    local_repo_dir.child(file_name).write_str("Local change")?;
    run_git_cmd(&["add", file_name], local_path)?;
    run_git_cmd(&["commit", "-m", "Local conflicting commit"], local_path)?;

    // 4. Create conflicting commit remotely
    let remote_clone_dir = setup_test_dir();
    run_git_cmd(&["clone", remote_path_str, "."], remote_clone_dir.path())?;
    remote_clone_dir.child(file_name).write_str("Remote change")?; // Different change
    run_git_cmd(&["add", file_name], remote_clone_dir.path())?;
    run_git_cmd(&["commit", "-m", "Remote conflicting commit"], remote_clone_dir.path())?;
    run_git_cmd(&["push", "origin", "main"], remote_clone_dir.path())?;

    // 5. Pull from the original local repo - expect failure
    let mut pull_cmd = Command::cargo_bin("arti-git")?;
    pull_cmd.current_dir(local_path)
            .arg("pull")
            .arg("origin")
            .arg("main")
            .assert()
            .failure() // Expect the command to fail due to conflict
            .stderr(predicate::str::contains("Merge conflict").and(predicate::str::contains(file_name)));

    // 6. Verify conflict markers in the file
    let file_content = local_repo_dir.child(file_name).read_to_string()?;
    assert!(file_content.contains("<<<<<<<"));
    assert!(file_content.contains("======="));
    assert!(file_content.contains(">>>>>>>"));
    assert!(file_content.contains("Local change"));
    assert!(file_content.contains("Remote change"));

    // 7. Verify merge state files exist (e.g., MERGE_HEAD)
    local_repo_dir.child(".git/MERGE_HEAD").assert(predicate::path::is_file());
    // TODO: Could also check index state if needed

    Ok(())
}


#[test]
fn test_clone_basic() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Setup remote repo with a commit
    let remote_repo_dir = setup_init_bare_repo()?;
    let remote_path_str = remote_repo_dir.path().to_str().expect("Remote path is not valid UTF-8");

    // Make initial commit in a temporary clone
    let temp_clone_dir = setup_test_dir();
    run_git_cmd(&["clone", remote_path_str, "."], temp_clone_dir.path())?;
    let file_name = "initial_file.txt";
    temp_clone_dir.child(file_name).write_str("Clonable content")?;
    run_git_cmd(&["add", file_name], temp_clone_dir.path())?;
    run_git_cmd(&["commit", "-m", "Initial commit for clone"], temp_clone_dir.path())?;
    run_git_cmd(&["push", "origin", "main"], temp_clone_dir.path())?;

    // 2. Setup clone target directory
    let clone_target_dir = setup_test_dir();
    let clone_target_path = clone_target_dir.path();

    // 3. Run arti-git clone
    let mut clone_cmd = Command::cargo_bin("arti-git")?;
    clone_cmd.current_dir(clone_target_path) // Run clone *into* the target dir
             .arg("clone")
             .arg(remote_path_str) // Source URL
             .arg(".") // Target directory (current dir)
             .assert()
             .success();
             // TODO: Assert stdout indicates successful clone

    // 4. Verify clone result
    clone_target_dir.child(".git").assert(predicate::path::is_dir());
    clone_target_dir.child(file_name).assert(predicate::path::is_file());
    clone_target_dir.child(file_name).assert(predicate::str::contains("Clonable content"));

    Ok(())
}


#[test]
fn test_status_basic() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Setup repo and initial commit
    let temp_dir = setup_init_repo()?;
    let repo_path = temp_dir.path();
    let file1_name = "file1.txt";
    let file2_name = "file2.txt";

    temp_dir.child(file1_name).write_str("Initial content for file 1")?;
    run_git_cmd(&["add", file1_name], repo_path)?;
    run_git_cmd(&["commit", "-m", "Commit file1"], repo_path)?;

    // 2. Modify file1, create file2
    temp_dir.child(file1_name).write_str("Modified content for file 1")?;
    temp_dir.child(file2_name).write_str("Content for file 2")?;

    // 3. Run status - expect modified and untracked
    let mut status_cmd1 = Command::cargo_bin("arti-git")?;
    status_cmd1.current_dir(repo_path)
               .arg("status")
               .assert()
               .success()
               .stdout(
                   predicate::str::contains(format!("modified:   {}", file1_name))
                   .and(predicate::str::contains("Untracked files:"))
                   .and(predicate::str::contains(file2_name))
               );

    // 4. Add both files
    run_git_cmd(&["add", file1_name, file2_name], repo_path)?;

    // 5. Run status again - expect staged changes
    let mut status_cmd2 = Command::cargo_bin("arti-git")?;
    status_cmd2.current_dir(repo_path)
               .arg("status")
               .assert()
               .success()
               .stdout(
                   predicate::str::contains("Changes to be committed:")
                   .and(predicate::str::contains(format!("modified:   {}", file1_name)))
                   .and(predicate::str::contains(format!("new file:   {}", file2_name)))
                   .and(predicate::str::contains("Untracked files:").not()) // No untracked files now
               );

    Ok(())
}