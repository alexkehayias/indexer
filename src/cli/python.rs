use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::PathBuf;

use crate::ai::tools::python::sandbox::shared_sandbox;

/// Run Python code in the embedded pybox WASM sandbox, mirroring the `python`
/// command line: either an inline program (`-c`) or a script file. The current
/// working directory is mounted as the sandbox root (`/`), so scripts can read
/// and write files there and nowhere else.
pub async fn run(command: Option<String>, file: Option<PathBuf>) -> Result<()> {
    let code = resolve_code(command, file)?;
    let workspace = std::env::current_dir().context("failed to read current directory")?;

    // The sandbox is CPU-bound and can run up to the timeout, so keep it off
    // the async runtime's worker threads.
    let result = tokio::task::spawn_blocking(move || {
        let sandbox = shared_sandbox()?;
        sandbox.exec(&code, &workspace)
    })
    .await?;

    match result {
        Ok(exec) => {
            let mut stdout = std::io::stdout();
            stdout.write_all(exec.stdout.as_bytes())?;
            stdout.flush()?;

            let mut stderr = std::io::stderr();
            stderr.write_all(exec.stderr.as_bytes())?;
            stderr.flush()?;

            Ok(())
        }
        Err(e) => {
            // A Python traceback from the guest. Print it raw and exit non-zero,
            // like the `python` command, rather than wrapping it as a CLI error.
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// Resolve the code to run from the mutually exclusive `-c`/file inputs.
fn resolve_code(command: Option<String>, file: Option<PathBuf>) -> Result<String> {
    match (command, file) {
        (Some(code), None) => Ok(code),
        (None, Some(path)) => std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display())),
        (Some(_), Some(_)) => bail!("cannot use -c together with a file argument"),
        (None, None) => bail!("no code provided: pass -c <code> or a script file"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn inline_command_is_used_verbatim() {
        let code = resolve_code(Some("1 + 1".to_string()), None).unwrap();
        assert_eq!(code, "1 + 1");
    }

    #[test]
    fn file_contents_are_read() {
        let path = std::env::temp_dir().join(format!("hq-python-{}.py", Uuid::new_v4()));
        std::fs::write(&path, "print('hi')").unwrap();

        let code = resolve_code(None, Some(path.clone())).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(code, "print('hi')");
    }

    #[test]
    fn rejects_both_command_and_file() {
        let err = resolve_code(Some("1".to_string()), Some(PathBuf::from("x.py"))).unwrap_err();
        assert!(err.to_string().contains("-c"));
    }

    #[test]
    fn rejects_no_input() {
        assert!(resolve_code(None, None).is_err());
    }
}
