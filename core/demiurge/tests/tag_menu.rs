// The tag menu both stacks show: Its width kept between its bounds and the
// menu inside its monitor, the entry under a point, and the selection
// stepping round.

use demiurge_core::config::Config;
use demiurge_core::tag_menu::{Entry, Icon, Menu, Style};

fn entries(titles: &[&str]) -> Vec<Entry> {
    titles
        .iter()
        .enumerate()
        .map(|(i, t)| Entry { id: i as u64, title: t.to_string(), minimized: i == 1, icon: None })
        .collect()
}

fn style() -> Style {
    Style::from_config(&Config::default()).expect("the bar's font and colors")
}

#[test]
fn the_menu_sizes_to_its_entries_and_stays_on_its_monitor() {
    let style = style();
    let short = Menu::new(&style, (100, 30), (0, 1280), entries(&["a", "b"]), None).unwrap();
    assert_eq!((short.x, short.y, short.w), (100, 30, 220), "a short list is the narrowest menu");
    let long = "x".repeat(400);
    let wide = Menu::new(&style, (100, 30), (0, 1280), entries(&[&long]), None).unwrap();
    assert_eq!(wide.w, 560, "a long title is cut at the widest menu");
    let edge = Menu::new(&style, (1200, 30), (0, 1280), entries(&["a"]), None).unwrap();
    assert_eq!(edge.x + edge.w, 1280, "a menu at the monitor's edge is kept on it");
    let second = Menu::new(&style, (1300, 30), (1280, 1920), entries(&["a"]), None).unwrap();
    assert_eq!(second.x, 1300, "a second monitor's menu keeps its own place");
}

#[test]
fn a_point_finds_its_entry_and_the_selection_wraps() {
    let style = style();
    let mut menu = Menu::new(&style, (0, 30), (0, 1280), entries(&["a", "b", "c"]), None).unwrap();
    let row = menu.height() / 3;
    assert_eq!(menu.entry_at(10, 30), Some(0));
    assert_eq!(menu.entry_at(10, 30 + row), Some(1));
    assert_eq!(menu.entry_at(10, 30 + menu.height()), None);
    assert_eq!(menu.entry_at(menu.w, 31), None);
    assert_eq!(menu.entry_at(10, 29), None);

    menu.step(false);
    assert_eq!(menu.selected, Some(2), "the first step up lands on the last entry");
    menu.step(true);
    assert_eq!(menu.selected, Some(0), "down from the last wraps to the first");
    let surface = menu.draw(&style).expect("the menu draws");
    assert_eq!((surface.width(), surface.height()), (menu.w, menu.height()));
}

// A window's icon is drawn in its row, beside the minimized mark, the menu
// wider by the icon.
#[test]
fn an_entry_with_an_icon_draws_it() {
    let style = style();
    let plain = Menu::new(&style, (0, 30), (0, 1280), entries(&["a long enough title for the menu to grow"]), None).unwrap();
    let mut with = entries(&["a long enough title for the menu to grow"]);
    // Solid magenta, premultiplied ARGB32 (B, G, R, A in memory).
    with[0].icon = Some(Icon { width: 32, height: 32, data: [0xff, 0x00, 0xff, 0xff].repeat(32 * 32) });
    let menu = Menu::new(&style, (0, 30), (0, 1280), with, None).unwrap();
    assert!(menu.w > plain.w, "the icon took no room");

    let mut surface = menu.draw(&style).expect("the menu draws");
    let stride = surface.stride() as usize;
    let data = surface.data().expect("the menu's pixels");
    let magenta = (0..menu.height() as usize).any(|y| {
        (0..menu.w as usize).any(|x| {
            let p = &data[y * stride + x * 4..y * stride + x * 4 + 3];
            p == [0xff, 0x00, 0xff]
        })
    });
    assert!(magenta, "the icon's pixels are not in the menu");
}
