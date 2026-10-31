// The settings Wayland GTK apps read: The `gsettings set` commands carrying
// DEMIURGE's [font], [cursor] and [appearance] into
// org.gnome.desktop.interface.

use demiurge_core::config::Config;
use demiurge_wl::xsettings::gsettings_commands;

#[test]
fn the_font_and_cursor_are_always_set_and_themes_when_named() {
    let mut cfg = Config::default();
    let keys = |cmds: &[String]| -> Vec<String> { cmds.iter().map(|c| c.split_whitespace().nth(3).unwrap().to_string()).collect() };
    assert_eq!(keys(&gsettings_commands(&cfg)), ["font-name", "cursor-theme", "cursor-size"]);
    cfg.appearance.gtk_theme = "Adwaita-dark".into();
    cfg.appearance.icon_theme = "Papirus-Dark".into();
    let cmds = gsettings_commands(&cfg);
    assert_eq!(keys(&cmds), ["font-name", "cursor-theme", "cursor-size", "gtk-theme", "icon-theme"]);
    assert_eq!(cmds[3], "gsettings set org.gnome.desktop.interface gtk-theme 'Adwaita-dark'");
}

#[test]
fn a_value_is_one_word_to_the_shell() {
    let mut cfg = Config::default();
    cfg.font.default = "It's Sans 10".into();
    let cmd = &gsettings_commands(&cfg)[0];
    assert_eq!(cmd, r"gsettings set org.gnome.desktop.interface font-name 'It'\''s Sans 10'");
    let out = std::process::Command::new("sh").arg("-c").arg(format!("printf %s {}", cmd.rsplit_once(" font-name ").unwrap().1)).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "It's Sans 10");
}
