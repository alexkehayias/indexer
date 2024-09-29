use std::process::Command;

// Clone a repo if it doesn't already exist
pub fn maybe_clone_repo(url: String, deploy_key_path: String) {
    let git_clone = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "GIT_SSH_COMMAND='ssh -i {} -o IdentitiesOnly=yes' git clone {}",
            deploy_key_path, url
        ))
        .output()
        .expect("failed to execute process");

    let stdout = std::str::from_utf8(&git_clone.stdout).expect("Failed to parse stdout");
    let stderr = std::str::from_utf8(&git_clone.stderr).expect("Failed to parse stderr");
    println!("stdout: {}\nstderr: {}", stdout, stderr);
}

// Pull and reset to origin main branch
pub fn maybe_pull_and_reset_repo(path: &str, deploy_key_path: String) {
    let git_clone = Command::new("sh")
        .arg("-c")
        .arg(format!("cd {} && GIT_SSH_COMMAND='ssh -i {} -o IdentitiesOnly=yes' git fetch origin && git reset --hard origin/main", path, deploy_key_path))
        .output()
        .expect("failed to execute process");

    let stdout = std::str::from_utf8(&git_clone.stdout).expect("Failed to parse stdout");
    let stderr = std::str::from_utf8(&git_clone.stderr).expect("Failed to parse stderr");
    tracing::debug!("stdout: {}\nstderr: {}", stdout, stderr);
}
