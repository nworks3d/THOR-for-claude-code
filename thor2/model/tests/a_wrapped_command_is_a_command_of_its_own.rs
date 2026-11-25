//! A command behind a wrapper (`timeout 5 deploy-prod`, `env -i FOO=1 deploy-prod`, `echo x |
//! xargs deploy-prod`, `find . -exec deploy-prod {} \;`) is seen as a command of its own, so a rule
//! about `deploy-prod` binds to it exactly as if it ran bare. Ordinary look-alikes that only carry or
//! mention other programs stay free.

use model::shell::{anchor_reaches, call_segments, WRAPPER_PROGRAMS};

fn reaches(command: &str, anchor: &str) -> bool {
    call_segments("Bash", command).iter().any(|s| anchor_reaches(anchor, s))
}

#[test]
fn the_wrapped_forms_are_caught() {
    for command in [
        "deploy-prod --now",
        "timeout 5 deploy-prod --now",
        "timeout -s KILL -k 3 5 deploy-prod --now",
        "env -i FOO=1 BAR=2 deploy-prod --now",
        "env FOO=1 deploy-prod --now",
        "nice -n 10 deploy-prod --now",
        "ionice -c 2 -n 5 deploy-prod --now",
        "stdbuf -oL deploy-prod --now",
        "stdbuf -o L -e 0 deploy-prod --now",
        "time deploy-prod --now",
        "sudo -u bob deploy-prod --now",
        "doas -u bob deploy-prod --now",
        "nohup deploy-prod --now",
        "exec deploy-prod --now",
        "setsid deploy-prod --now",
        "echo x | xargs deploy-prod --now",
        "echo x | xargs -n1 -I{} deploy-prod {}",
        "echo x | xargs -0 -P 4 deploy-prod",
        "echo x | xargs -d '\\n' deploy-prod",
        "find . -exec deploy-prod {} \\;",
        "find . -name x -execdir deploy-prod --now {} +",
        "find . -ok deploy-prod {} \\;",
        "timeout 5 env -i nice -n 1 sudo deploy-prod --now",
        "cd /srv && echo x | xargs deploy-prod",
        "bash -c 'echo x | xargs deploy-prod'",
    ] {
        assert!(reaches(command, "deploy-prod"), "{command}");
    }
}

#[test]
fn the_look_alikes_stay_free() {
    for command in [
        "timeout 5 cargo test",
        "env FOO=1 npm test",
        "find . -name x -exec grep y {} \\;",
        "find . -name deploy-prod",
        "xargs grep deploy-prod",
        "echo deploy-prod | xargs grep y",
        "time git status",
        "grep deploy-prod notes.txt",
        "echo deploy-prod",
        "stdbuf -oL cat deploy-prod.log",
        "nice -n 5 make",
    ] {
        assert!(!reaches(command, "deploy-prod"), "{command}");
    }
    // The wrapped program is its own command: the wrapper itself is no longer the head.
    assert!(reaches("timeout 5 cargo test", "cargo test") && reaches("env FOO=1 npm test", "npm test") && reaches("time git status", "git status"));
    assert!(reaches("find . -name x -exec grep y {} \\;", "grep y"));
}

#[test]
fn there_is_one_wrapper_list() {
    for name in ["timeout", "env", "nice", "stdbuf", "time", "sudo", "doas", "nohup", "exec", "xargs"] {
        assert!(WRAPPER_PROGRAMS.contains(&name), "{name}");
    }
}
