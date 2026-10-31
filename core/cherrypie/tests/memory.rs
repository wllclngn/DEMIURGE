// Window memory: Where the user leaves a window is written into the rule that
// decides its geometry, as a monitor name plus percentages of the monitor's
// work area, with the rest of the file kept as the user wrote it.

use cherrypie::api::{NewWindow, Rect};
use cherrypie::learn;
use cherrypie::memory::{record, Spot};
use cherrypie::place::{self, MonitorGeometry};

const AREA: Rect = Rect { x: 0, y: 40, w: 2560, h: 2840 };

const CONFIG: &str = "\
# my rules
[[rule]]
class = '^Hand$'
position = \"center\"
size = [\"50%\", \"50%\"]

# Learned 2026-10-16 from a new window; edit to taste.
[[rule]]
class = '^Learned$'
position = \"center\"
size = [\"60%\", \"70%\"]  # roomy
focus = true

[[rule]]
class = '^Learned$'
workspace = \"Z\"
";

fn window(class: &str) -> NewWindow {
    NewWindow {
        window: 1,
        class: class.into(),
        title: class.into(),
        role: String::new(),
        process: String::new(),
        window_type: "normal".into(),
        transient: false,
        requested: Rect { x: 0, y: 0, w: 300, h: 200 },
        fullscreen: false,
        monitor: "HDMI-0".into(),
    }
}

fn spot(x: i32, y: i32, w: u32, h: u32) -> Spot {
    Spot::new("HDMI-0", Rect { x, y, w, h }, AREA)
}

#[test]
fn a_learned_rule_remembers_and_the_rest_stays_verbatim() {
    let out = record(CONFIG, &window("Learned"), &spot(256, 140, 1280, 1420)).unwrap();
    let expected = CONFIG.replace(
        "position = \"center\"\nsize = [\"60%\", \"70%\"]  # roomy\nfocus = true\n",
        "position = [\"10%\", \"3.52%\"]\nsize = [\"50%\", \"50%\"]  # roomy\nfocus = true\nmonitor = \"HDMI-0\"\n",
    );
    assert_eq!(out.as_deref(), Some(expected.as_str()), "the tag-only rule after it must not take the spot");
}

#[test]
fn a_hand_written_rule_remembers_only_when_it_says_so() {
    let at = spot(0, 40, 640, 480);
    assert_eq!(record(CONFIG, &window("Hand"), &at), Ok(None));

    let opted = CONFIG.replace("class = '^Hand$'\n", "class = '^Hand$'\nremember = true\n");
    let out = record(&opted, &window("Hand"), &at).unwrap().expect("remember = true");
    assert!(out.contains("remember = true\nposition = [\"0%\", \"0%\"]\nsize = [\"25%\", \"16.9%\"]\nmonitor = \"HDMI-0\"\n"), "{out}");

    let declined = CONFIG.replace("class = '^Learned$'\nposition", "class = '^Learned$'\nremember = false\nposition");
    assert_eq!(record(&declined, &window("Learned"), &at), Ok(None));
}

#[test]
fn nothing_is_written_without_a_rule_or_a_change() {
    let at = spot(100, 100, 800, 600);
    assert_eq!(record(CONFIG, &window("Stranger"), &at), Ok(None));
    let once = record(CONFIG, &window("Learned"), &at).unwrap().unwrap();
    assert_eq!(record(&once, &window("Learned"), &at), Ok(None));
}

// Every offset and size on monitors up to 3840 px comes back to the same
// pixel through the rule the spot is written into.
#[test]
fn a_spot_lands_back_on_the_same_pixels() {
    for total in [1080u32, 2840, 3840] {
        let area = Rect { x: 0, y: 0, w: total, h: total };
        let geometry = MonitorGeometry { name: "M".into(), x: 0, y: 0, width: total, height: total };
        for px in 0..=total as i32 {
            let size = px.max(1) as u32;
            let s = Spot::new("M", Rect { x: px, y: px, w: size, h: size }, area);
            let text = format!("[[rule]]\nclass = 'x'\nposition = {:?}\nsize = {:?}\n", s.position, s.size);
            let rule = learn::compile(&text).unwrap();
            assert_eq!(
                place::geometry(&rule, &geometry, (0, 0, 1, 1)),
                Some((px, px, size, size)),
                "{text}"
            );
        }
    }
}
