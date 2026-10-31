use std::time::{Duration, Instant};

use demiurge_core::bar::{CLOCK_MAX_W, PROMPT_MAX_W, Painted, Painter, Panel, Rect, Region, TAGS_EDGE_PAD, View};
use demiurge_core::config::Config;
use demiurge_core::prompt::Prompt;

const REGIONS: [Region; 4] = [Region::Tags, Region::Prompt, Region::Title, Region::Clock];

fn painter(clock_format: &str) -> Painter {
    let mut config = Config::default();
    config.bar.clock_format = clock_format.to_string();
    Painter::new(&config).expect("the bar's font and colors")
}

fn view<'a>(occupied: &'a [bool], prompt: Option<&'a Prompt>, center: Option<(&'a str, bool)>) -> View<'a> {
    View { active_tag: 0, occupied, urgent: &[], prompt, center }
}

fn rects(painted: Painted, panel: &Panel) -> Vec<Rect> {
    painted.rects(panel).collect()
}

fn pixels(panel: &Panel) -> (Vec<u8>, usize) {
    panel.with_pixels(|data, stride| (data.to_vec(), stride)).expect("the bar's pixels")
}

// Whether any pixel outside `rect` differs between two drawings of a bar.
fn changed_outside(before: &(Vec<u8>, usize), after: &(Vec<u8>, usize), rect: Rect) -> bool {
    let stride = before.1;
    (0..before.0.len() / stride).any(|y| {
        (0..stride / 4).any(|x| {
            let inside = (x as i32) >= rect.x && (x as i32) < rect.x + rect.w && (y as i32) >= rect.y && (y as i32) < rect.y + rect.h;
            let at = y * stride + x * 4;
            !inside && before.0[at..at + 3] != after.0[at..at + 3]
        })
    })
}

#[test]
fn a_wide_bar_gives_each_region_its_room() {
    let painter = painter("%H");
    let panel = painter.panel(2560).unwrap();
    let tags_end = painter.tag_extents().last().unwrap().1 + TAGS_EDGE_PAD;
    assert_eq!(painter.tag_extents()[0].0, TAGS_EDGE_PAD);
    assert_eq!(panel.rect(Region::Tags), Rect { x: 0, y: 0, w: tags_end, h: 30 });
    assert_eq!(panel.rect(Region::Prompt), Rect { x: tags_end, y: 0, w: PROMPT_MAX_W, h: 30 });
    assert_eq!(panel.rect(Region::Clock), Rect { x: 2560 - CLOCK_MAX_W, y: 0, w: CLOCK_MAX_W, h: 30 });
    let title = panel.rect(Region::Title);
    assert_eq!((title.x, title.x + title.w), (tags_end + PROMPT_MAX_W, 2560 - CLOCK_MAX_W));
}

#[test]
fn the_regions_split_any_bar_without_overlapping() {
    let painter = painter("%H");
    for width in [2560, 1280, 900, 300, 1] {
        let panel = painter.panel(width).unwrap();
        let mut x = 0;
        for region in REGIONS {
            let rect = panel.rect(region);
            assert_eq!(rect.x, x, "{:?} on a {} wide bar", region, width);
            assert!(rect.w >= 0, "{:?} on a {} wide bar", region, width);
            x += rect.w;
        }
        assert_eq!(x, width as i32);
        assert_eq!(panel.rect(Region::Clock).w, CLOCK_MAX_W.min(width as i32));
    }
}

#[test]
fn a_bar_is_drawn_whole_once_then_not_again_unchanged() {
    let painter = painter("%H");
    let mut panel = painter.panel(1920).unwrap();
    let occupied = [true, false, false];
    let first = painter.paint(&mut panel, &view(&occupied, None, Some(("kitty", false))));
    assert_eq!(rects(first, &panel), vec![Rect { x: 0, y: 0, w: 1920, h: 30 }]);
    let again = painter.paint(&mut panel, &view(&occupied, None, Some(("kitty", false))));
    assert!(again.is_empty());
}

#[test]
fn each_change_draws_its_own_region_alone() {
    let painter = painter("%H");
    let mut panel = painter.panel(1920).unwrap();
    painter.paint(&mut panel, &view(&[false, false], None, None));

    let painted = painter.paint(&mut panel, &view(&[false, true], None, None));
    assert_eq!(rects(painted, &panel), vec![panel.rect(Region::Tags)]);

    let urgent = [false, true];
    let painted = painter.paint(&mut panel, &View { urgent: &urgent, ..view(&[false, true], None, None) });
    assert_eq!(rects(painted, &panel), vec![panel.rect(Region::Tags)]);
    painter.paint(&mut panel, &view(&[false, true], None, None));

    let prompt = Prompt { input: "fire".into(), cursor: 4, completion: Some("firefox".into()) };
    let painted = painter.paint(&mut panel, &view(&[false, true], Some(&prompt), None));
    assert_eq!(rects(painted, &panel), vec![panel.rect(Region::Prompt)]);

    let painted = painter.paint(&mut panel, &view(&[false, true], Some(&prompt), Some(("saved", true))));
    assert_eq!(rects(painted, &panel), vec![panel.rect(Region::Title)]);

    let painted = painter.paint(&mut panel, &view(&[false, true], None, None));
    assert_eq!(rects(painted, &panel), vec![panel.rect(Region::Prompt), panel.rect(Region::Title)]);
}

#[test]
fn a_clock_tick_redraws_the_clock_and_nothing_else() {
    let mut painter = painter("%S");
    let mut panel = painter.panel(1920).unwrap();
    painter.paint(&mut panel, &view(&[true], None, Some(("kitty", false))));
    let before = pixels(&panel);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !painter.tick_clock() {
        assert!(Instant::now() < deadline, "the clock never showed a new second");
        std::thread::sleep(Duration::from_millis(20));
    }
    let painted = painter.paint(&mut panel, &view(&[true], None, Some(("kitty", false))));
    let clock = panel.rect(Region::Clock);
    assert_eq!(rects(painted, &panel), vec![clock]);
    let after = pixels(&panel);
    assert!(!changed_outside(&before, &after, clock));
    assert_ne!(before.0, after.0, "the clock's new second was not drawn");
}

#[test]
fn a_long_prompt_stays_inside_its_region() {
    let painter = painter("%H");
    let mut panel = painter.panel(1920).unwrap();
    painter.paint(&mut panel, &view(&[], None, None));
    let before = pixels(&panel);
    let long = "x".repeat(300);
    let prompt = Prompt { cursor: long.len(), input: long, completion: None };
    painter.paint(&mut panel, &view(&[], Some(&prompt), None));
    let after = pixels(&panel);
    assert!(!changed_outside(&before, &after, panel.rect(Region::Prompt)));
    assert_ne!(before.0, after.0, "the prompt was not drawn");
}
