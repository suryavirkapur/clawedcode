use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Cargo,
    Npm,
    LocalBuild,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePlan {
    pub method: InstallMethod,
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOutcome {
    pub method: InstallMethod,
    pub command: String,
}

pub fn render_update_command(program: &str, args: &[String]) -> String {
    if args.is_empty() {
        program.to_string()
    } else {
        format!("{program} {}", args.join(" "))
    }
}

pub fn detect_install_method() -> InstallMethod {
    match std::env::var("CLAWEDCODE_INSTALL_METHOD").ok().as_deref() {
        Some("npm") => InstallMethod::Npm,
        Some("cargo") => InstallMethod::Cargo,
        Some("local") => InstallMethod::LocalBuild,
        _ => std::env::current_exe()
            .ok()
            .as_deref()
            .map(detect_install_method_from_path)
            .unwrap_or(InstallMethod::Unknown),
    }
}

pub fn detect_install_method_from_path(path: &Path) -> InstallMethod {
    let path_str = path.to_string_lossy();
    if path_str.contains("/target/debug/")
        || path_str.contains("/target/release/")
        || path_str.contains("\\target\\debug\\")
        || path_str.contains("\\target\\release\\")
    {
        return InstallMethod::LocalBuild;
    }

    let parent = path.parent().and_then(|p| p.to_str()).unwrap_or_default();
    if parent.ends_with("/.cargo/bin")
        || parent.ends_with("\\.cargo\\bin")
        || parent.contains("/.cargo/bin/")
        || parent.contains("\\.cargo\\bin\\")
    {
        return InstallMethod::Cargo;
    }

    if path_str.contains("node_modules")
        || parent.contains("npm")
        || parent.contains("pnpm")
        || parent.contains("yarn")
    {
        return InstallMethod::Npm;
    }

    InstallMethod::Unknown
}

pub fn plan_self_update() -> Result<UpdatePlan> {
    match detect_install_method() {
        InstallMethod::Cargo => Ok(UpdatePlan {
            method: InstallMethod::Cargo,
            program: "cargo".to_string(),
            args: vec![
                "install".to_string(),
                "clawedcode".to_string(),
                "--force".to_string(),
            ],
        }),
        InstallMethod::Npm => Ok(UpdatePlan {
            method: InstallMethod::Npm,
            program: npm_program_name(),
            args: vec![
                "install".to_string(),
                "-g".to_string(),
                "clawedcode@latest".to_string(),
            ],
        }),
        InstallMethod::LocalBuild => bail!(
            "self-update is not supported for a local build; rebuild locally or install via cargo or npm"
        ),
        InstallMethod::Unknown => bail!(
            "could not determine install method; set CLAWEDCODE_INSTALL_METHOD to cargo or npm"
        ),
    }
}

pub fn run_self_update() -> Result<UpdateOutcome> {
    let plan = plan_self_update()?;
    let command = render_update_command(&plan.program, &plan.args);
    let status = Command::new(&plan.program)
        .args(&plan.args)
        .status()
        .with_context(|| format!("failed to run {}", plan.program))?;

    if !status.success() {
        bail!(
            "update command `{}` exited with status {}",
            command,
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string())
        );
    }

    Ok(UpdateOutcome {
        method: plan.method,
        command,
    })
}

fn npm_program_name() -> String {
    if cfg!(windows) {
        "npm.cmd".to_string()
    } else {
        "npm".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env_lock;
    use std::path::PathBuf;

    #[test]
    fn detect_install_method_marks_local_debug_build() {
        let path = PathBuf::from("/tmp/clawedcode/target/debug/clawedcode");
        assert_eq!(
            detect_install_method_from_path(&path),
            InstallMethod::LocalBuild
        );
    }

    #[test]
    fn detect_install_method_marks_cargo_bin() {
        let path = PathBuf::from("/home/sk/.cargo/bin/clawedcode");
        assert_eq!(detect_install_method_from_path(&path), InstallMethod::Cargo);
    }

    #[test]
    fn detect_install_method_marks_npm_wrapper() {
        let path = PathBuf::from("/usr/lib/node_modules/clawedcode/bin/clawedcode");
        assert_eq!(detect_install_method_from_path(&path), InstallMethod::Npm);
    }

    #[test]
    fn plan_self_update_uses_cargo_install_when_detected() {
        let _guard = env_lock();
        unsafe { std::env::set_var("CLAWEDCODE_INSTALL_METHOD", "cargo") };

        let plan = plan_self_update().expect("cargo plan");
        assert_eq!(plan.method, InstallMethod::Cargo);
        assert_eq!(plan.program, "cargo");
        assert_eq!(
            plan.args,
            vec![
                "install".to_string(),
                "clawedcode".to_string(),
                "--force".to_string()
            ]
        );

        unsafe { std::env::remove_var("CLAWEDCODE_INSTALL_METHOD") };
    }

    #[test]
    fn plan_self_update_uses_npm_install_when_detected() {
        let _guard = env_lock();
        unsafe { std::env::set_var("CLAWEDCODE_INSTALL_METHOD", "npm") };

        let plan = plan_self_update().expect("npm plan");
        assert_eq!(plan.method, InstallMethod::Npm);
        assert_eq!(plan.program, npm_program_name());
        assert_eq!(
            plan.args,
            vec![
                "install".to_string(),
                "-g".to_string(),
                "clawedcode@latest".to_string()
            ]
        );

        unsafe { std::env::remove_var("CLAWEDCODE_INSTALL_METHOD") };
    }

    #[test]
    fn render_update_command_joins_program_and_args() {
        let command = render_update_command(
            "cargo",
            &["install".to_string(), "clawedcode".to_string(), "--force".to_string()],
        );

        assert_eq!(command, "cargo install clawedcode --force");
    }

    #[test]
    fn run_self_update_reports_attempted_command_on_failure() {
        let _guard = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "clawed_update_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let script = dir.join("cargo");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf 'update stderr\\n' >&2\nexit 7\n",
        )
        .expect("write script");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).expect("set perms");
        }

        unsafe { std::env::set_var("CLAWEDCODE_INSTALL_METHOD", "cargo") };
        let original_path = std::env::var_os("PATH");
        let new_path = match &original_path {
            Some(path) => format!("{}:{}", dir.display(), path.to_string_lossy()),
            None => dir.display().to_string(),
        };
        unsafe { std::env::set_var("PATH", new_path) };

        let err = run_self_update().expect_err("failing update should error");
        let message = err.to_string();
        assert!(message.contains("update command `cargo install clawedcode --force` exited with status"));

        match original_path {
            Some(path) => unsafe { std::env::set_var("PATH", path) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        unsafe { std::env::remove_var("CLAWEDCODE_INSTALL_METHOD") };
        std::fs::remove_dir_all(&dir).ok();
    }
}
