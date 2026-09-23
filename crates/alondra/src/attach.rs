use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::{Context, Result};

pub fn attach(short_id: &str) -> Result<()> {
    let mut cmd = Command::new("claude");
    cmd.arg("attach").arg(short_id).current_dir("/tmp");
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpid());
            libc::signal(libc::SIGTTOU, libc::SIG_DFL);
            Ok(())
        });
    }
    let child = cmd.spawn().context("no pude lanzar claude attach")?;
    let pid = child.id() as libc::pid_t;
    unsafe {
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        libc::setpgid(pid, pid);
        libc::tcsetpgrp(libc::STDIN_FILENO, pid);
    }
    loop {
        let mut status = 0;
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) };
        if r < 0 {
            break;
        }
        if libc::WIFSTOPPED(status) {
            unsafe {
                libc::kill(pid, libc::SIGTERM);
                libc::kill(pid, libc::SIGCONT);
            }
            continue;
        }
        if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
            break;
        }
    }
    unsafe {
        libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpgrp());
        libc::signal(libc::SIGTTOU, libc::SIG_DFL);
    }
    Ok(())
}
