use std::process::Command;

/// Clone a repo if it doesn't already exist
pub fn maybe_clone_repo(deploy_key_path: &str, url: &str, storage_path: &str) {
    let git_clone = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "GIT_SSH_COMMAND='ssh -i {} -o IdentitiesOnly=yes' git clone {} {}",
            deploy_key_path, url, storage_path
        ))
        .output()
        .expect("failed to execute process");

    let stdout = std::str::from_utf8(&git_clone.stdout).expect("Failed to parse stdout");
    let stderr = std::str::from_utf8(&git_clone.stderr).expect("Failed to parse stderr");
    println!("stdout: {}\nstderr: {}", stdout, stderr);
}

/// Pull and reset to origin main branch
pub fn maybe_pull_and_reset_repo(deploy_key_path: &str, path: &str) {
    let git_clone = Command::new("sh")
        .arg("-c")
        .arg(format!("cd {} && GIT_SSH_COMMAND='ssh -i {} -o IdentitiesOnly=yes' git fetch origin && git reset --hard origin/main", path, deploy_key_path))
        .output()
        .expect("Failed to execute process");

    let stdout = std::str::from_utf8(&git_clone.stdout).expect("Failed to parse stdout");
    let stderr = std::str::from_utf8(&git_clone.stderr).expect("Failed to parse stderr");
    tracing::debug!("stdout: {}\nstderr: {}", stdout, stderr);
}

/// Return a list of files that have changed between the last two
/// commits.  Run `maybe_pull_and_reset_repo` before hand if you want
/// to get a list of files that changed on origin.
pub fn diff_last_commit_files(deploy_key_path: &str, path: &str) -> Vec<String> {
    // Run git diff
    let command = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "cd {} && GIT_SSH_COMMAND='ssh -i {} -o IdentitiesOnly=yes' git --no-pager diff --name-only HEAD^ HEAD",
            path,
            deploy_key_path
        ))
        .output()
        .expect("Failed to execute process");

    let stdout = std::str::from_utf8(&command.stdout).expect("Failed to parse stdout");
    let stderr = std::str::from_utf8(&command.stderr).expect("Failed to parse stderr");

    if !stderr.is_empty() {
        tracing::error!("Git diff failed: {}", stderr);
    }

    stdout.split("\n").map(|s| s.to_string()).collect()
}
