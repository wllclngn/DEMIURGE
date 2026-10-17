// Which spawn commands need a shell: Pipes, redirects, variables, globs and
// tildes do; plain argument lists run directly.

use demiurge_x11::spawn::needs_shell;

#[test]
fn simple_command_direct() {
    assert!(!needs_shell("kitty"));
    assert!(!needs_shell("physlock"));
}

#[test]
fn multi_arg_no_metachars_direct() {
    assert!(!needs_shell("brightnessctl set 5%+"));
    assert!(!needs_shell("brightnessctl set 5%-"));
    assert!(!needs_shell("wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+"));
    assert!(!needs_shell("wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle"));
}

#[test]
fn dollar_var_needs_shell() {
    assert!(needs_shell("flameshot gui -p $HOME"));
    assert!(needs_shell("flameshot full -p $HOME"));
}

#[test]
fn pipe_needs_shell() {
    assert!(needs_shell("echo hello | grep o"));
}

#[test]
fn redirect_needs_shell() {
    assert!(needs_shell("echo hi > /tmp/out"));
}

#[test]
fn tilde_needs_shell() {
    assert!(needs_shell("kitty -d ~"));
}

#[test]
fn glob_needs_shell() {
    assert!(needs_shell("rm /tmp/foo*"));
}

#[test]
fn semicolon_needs_shell() {
    assert!(needs_shell("a ; b"));
}
